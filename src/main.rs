mod api;
mod player;
mod renderer;
mod state;
mod ui;

use api::*;
use state::*;

use rand::Rng;
use std::io::{stdout, Write};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;

use ratatui::{backend::CrosstermBackend, Terminal};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use souvlaki::{MediaControlEvent, MediaMetadata, MediaPlayback};

fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(stdout(), DisableMouseCapture, Show, LeaveAlternateScreen);
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        restore_terminal();
        original_hook(panic_info);
    }));

    let mut state = AppState::new(vec![]);
    let mut out = stdout();

    execute!(
        out,
        EnterAlternateScreen,
        Clear(ClearType::All),
        Hide,
        EnableMouseCapture
    )?;
    terminal::enable_raw_mode()?;

    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let (tx_track, mut rx_track) = tokio::sync::mpsc::channel::<TrackInfo>(100);
    let (tx_progress, mut rx_progress) = tokio::sync::mpsc::channel::<f64>(100);

    let (tx_lyrics, mut rx_lyrics) = tokio::sync::mpsc::channel::<(String, Result<String>)>(10);
    let (tx_player_status, mut rx_player_status) = tokio::sync::mpsc::channel::<String>(10);

    let pause_signal = state.fetch_paused.clone();
    let tx_fetch_status = tx_player_status.clone();
    tokio::spawn(async move {
        if let Err(e) = fetch_tracks_streaming(tx_track, tx_progress, pause_signal).await {
            let _ = tx_fetch_status
                .send(format!("Error fetching tracks: {}", e))
                .await;
        }
    });
    let (tx_album_art, mut rx_album_art) = tokio::sync::mpsc::channel::<Vec<u8>>(10);
    let (tx_video_frame, mut rx_video_frame) =
        tokio::sync::mpsc::channel::<(Vec<u8>, u32, u32)>(30);

    let mut video_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut video_frame_count: u64 = 0;
    let mut last_tick = Instant::now();
    let mut last_key: Option<(KeyCode, Instant)> = None;
    let mut last_media_publish = Instant::now();
    // フレームレート制限: 目標 ~30fps = 33ms/frame
    const FRAME_DURATION: Duration = Duration::from_millis(33);
    let mut frame_start: Instant;

    loop {
        frame_start = Instant::now();
        terminal.draw(|f| ui::draw_ui(f, &mut state))?;

        // 5秒間操作がなければ歌詞のスクロールをリセット
        if state.lyric_scroll_offset != 0
            && state.last_lyric_interaction.elapsed() > Duration::from_secs(5)
        {
            state.lyric_scroll_offset = 0;
        }

        // Handle Media Control Events (OS) via Channel
        while let Ok(event) = state.rx_media_events.try_recv() {
            match event {
                // デスクトップ側からのシーク(ame-shell の歌詞の行のクリックなど)
                MediaControlEvent::SetPosition(souvlaki::MediaPosition(pos)) => {
                    if state.playing_id.is_some() {
                        player::seek_to(pos.as_secs_f64());
                        publish_media(&mut state);
                    }
                }
                MediaControlEvent::SeekBy(dir, by) => {
                    if state.playing_id.is_some() {
                        let secs = by.as_secs_f64();
                        player::seek_relative(if matches!(dir, souvlaki::SeekDirection::Forward) {
                            secs
                        } else {
                            -secs
                        });
                        publish_media(&mut state);
                    }
                }
                MediaControlEvent::Play | MediaControlEvent::Pause | MediaControlEvent::Toggle => {
                    if state.is_playing_video {
                        state.is_playing_video = false;
                        state.video_rgb = None;
                        state.video_cells = None;
                        if let Some(task) = video_task.take() {
                            task.abort();
                        }
                    }
                    if state.playing_id.is_some() {
                        // Play は止まっているときだけ、Pause は鳴っているときだけ切り替える
                        // (以前はどれも反転していたので、再生中に Play が届くと止まっていた)
                        let want_pause = match event {
                            MediaControlEvent::Play => false,
                            MediaControlEvent::Pause => true,
                            _ => !state.is_paused,
                        };
                        if want_pause != state.is_paused {
                            state.is_paused = player::toggle_pause();
                        }
                        state.last_action = if state.is_paused {
                            "⏸".into()
                        } else {
                            "▶".into()
                        };
                        publish_media(&mut state);
                    }
                }
                MediaControlEvent::Next => {
                    if state.is_shuffle && state.filtered_indices.len() > 1 {
                        state.current =
                            rand::thread_rng().gen_range(0..state.filtered_indices.len());
                        state.list_state.select(Some(state.current));
                        state.last_action = "🔀".into();
                        play_selected_track(
                            &mut state,
                            tx_lyrics.clone(),
                            tx_player_status.clone(),
                            tx_album_art.clone(),
                            &mut video_task,
                        );
                    } else if state.current < state.filtered_indices.len().saturating_sub(1) {
                        state.current += 1;
                        state.list_state.select(Some(state.current));
                        state.last_action = "⏭".into();
                        play_selected_track(
                            &mut state,
                            tx_lyrics.clone(),
                            tx_player_status.clone(),
                            tx_album_art.clone(),
                            &mut video_task,
                        );
                    }
                }
                MediaControlEvent::Previous => {
                    if state.current > 0 {
                        state.current -= 1;
                        state.list_state.select(Some(state.current));
                        state.last_action = "⏮".into();
                        play_selected_track(
                            &mut state,
                            tx_lyrics.clone(),
                            tx_player_status.clone(),
                            tx_album_art.clone(),
                            &mut video_task,
                        );
                    }
                }
                _ => {}
            }
        }

        while let Ok(msg) = rx_player_status.try_recv() {
            if msg == "Playing" {
                state.is_actually_playing = true;
                let title = state
                    .tracks
                    .iter()
                    .find(|t| Some(&t.path) == state.playing_id.as_ref())
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                state.status_msg = format!("Playing: {}", title);
                if state.parsed_lyrics.is_empty() {
                    state.current_lyric = "● Playing...".into();
                }

                // Update system media status
                publish_media(&mut state);
            } else if msg.contains("Error") {
                state.error_msg = Some(msg.clone());
                state.current_lyric = format!("❌ {}", msg);
                state.is_actually_playing = false;
            } else {
                state.status_msg = msg.clone();
                state.current_lyric = format!(">> {}", msg);
            }
        }

        while let Ok(art_data) = rx_album_art.try_recv() {
            if let Ok(img) = image::load_from_memory(&art_data) {
                state.album_art = Some(img);
                // アートが変わったのでprotocolキャッシュを無効化
                state.album_art_protocol = None;

                // システム通知用に一時ファイルに保存(曲ごとに別の名前。前の曲のファイルは消す)
                let temp_path = art_path_for(state.playing_id.as_deref().unwrap_or(""));
                if let Ok(mut file) = std::fs::File::create(&temp_path) {
                    let _ = file.write_all(&art_data);
                    let new_path = temp_path.to_string_lossy().to_string();
                    if let Some(old) = state.art_temp_path.take() {
                        if old != new_path {
                            let _ = std::fs::remove_file(old);
                        }
                    }
                    state.art_temp_path = Some(new_path);
                    // ジャケットが届いたことを OS のメディア連携にも伝える
                    publish_media(&mut state);
                }
            }
        }

        while let Ok((pixels, w, h)) = rx_video_frame.try_recv() {
            state.video_rgb = Some((pixels, w, h));
            state.video_frame_dirty = true;
            video_frame_count += 1;
            if state.is_playing_video {
                state.video_playback_pos = video_frame_count as f64 / 15.0;
            }
        }

        while let Ok(p) = rx_progress.try_recv() {
            state.load_progress = p;
        }
        let mut loaded = false;
        while let Ok(track) = rx_track.try_recv() {
            state.tracks.push(track);
            loaded = true;
        }
        if loaded {
            state.is_loading = state.load_progress < 99.9;
            if !state.is_loading {
                state.finalize_loading();
            }
            state.update_search();
            if state.tracks.len() == 1 {
                state.list_state.select(Some(0));
            }
        }

        while let Ok((path, result)) = rx_lyrics.try_recv() {
            if state.playing_id.as_ref() == Some(&path) {
                match result {
                    Ok(lrc) => {
                        state.parsed_lyrics = parse_lrc(&lrc);
                        if state.parsed_lyrics.is_empty() {
                            state.current_lyric = "(No time tags)".into();
                        }
                        // デスクトップ側(ame-shell のダッシュボード)にも、この曲の歌詞を渡す
                        if let Some(t) = state.tracks.iter().find(|t| t.path == path) {
                            write_lyrics_sidecar(&t.title, &t.artist, &t.album, Some(&lrc));
                        }
                    }
                    Err(_) => {
                        state.current_lyric = "(No lyrics found)".into();
                    }
                }
            }
        }

        // 再生中は 1 秒ごとに再生位置を OS のメディア連携へ(ダッシュボードの進み具合・同期歌詞のため)
        if state.playing_id.is_some()
            && !state.is_paused
            && last_media_publish.elapsed() >= Duration::from_secs(1)
        {
            last_media_publish = Instant::now();
            publish_media(&mut state);
        }

        // フレームレート制限: 残り時間スリープ (ビジーループ防止)
        let elapsed = frame_start.elapsed();
        if elapsed < FRAME_DURATION {
            let sleep_dur = FRAME_DURATION - elapsed;
            // poll でイベント待機しながらスリープ (最大で残り時間)
            if event::poll(sleep_dur)? {
                let ev = event::read()?;
                match ev {
                    Event::Key(key) => {
                        let now = Instant::now();
                        match state.input_mode {
                            InputMode::Normal => match key.code {
                                KeyCode::Char('v') => {
                                    if state.is_playing_video {
                                        state.is_playing_video = false;
                                        state.video_rgb = None;
                                        state.video_cells = None;
                                        if let Some(task) = video_task.take() {
                                            task.abort();
                                        }
                                        state.last_action = "📜 Lyrics".into();

                                        // Resume music automatically when returning from video
                                        if state.is_paused {
                                            state.is_paused = player::toggle_pause();
                                        }
                                    } else {
                                        let video_info = state
                                            .current_track()
                                            .and_then(|t| t.video.as_ref().map(|v| v.clone()));

                                        if let Some(v_path) = video_info {
                                            let url = video_url_from_path(&v_path);

                                            // Music -> Stop
                                            player::pause();
                                            state.is_paused = true;

                                            state.is_playing_video = true;
                                            state.last_action = "🎬 Video Mode".into();

                                            if let Some(task) = video_task.take() {
                                                task.abort();
                                            }
                                            // Start from 0.0 as durations may differ
                                            video_frame_count = 0;
                                            state.video_playback_pos = 0.0;
                                            video_task = Some(spawn_video_task(
                                                url,
                                                0.0,
                                                tx_video_frame.clone(),
                                            ));
                                        }
                                    }
                                }
                                KeyCode::Char('V') => {
                                    let video_info = state
                                        .current_track()
                                        .and_then(|t| t.video.as_ref().map(|v| v.clone()));

                                    if let Some(v_path) = video_info {
                                        let url = video_url_from_path(&v_path);
                                        player::pause();
                                        state.is_paused = true;
                                        state.last_action = "🎬 Ext Video (MPV)".into();

                                        let mut cmd = std::process::Command::new("mpv");
                                        cmd.arg("--ytdl=no");
                                        cmd.arg("--force-window");
                                        cmd.arg("--user-agent=Mozilla/5.0");
                                        cmd.arg(url);
                                        let _ = cmd.spawn();
                                    }
                                }
                                KeyCode::Char('f') => {
                                    state.toggle_favorite();
                                    state.last_action = "⭐ Fav".into();
                                }
                                KeyCode::Char('s') => {
                                    state.is_shuffle = !state.is_shuffle;
                                    state.last_action = "🔀 Shuffle".into();
                                }
                                KeyCode::Char('p') => {
                                    if state.current_track().is_some() {
                                        state.input_mode = InputMode::PlaylistInput;
                                        state.playlist_input.clear();
                                        state.last_action = "Playlist".into();
                                    }
                                }
                                KeyCode::Char('F') => {
                                    state.toggle_favorite_view();
                                    state.last_action = if state.show_favorites_only {
                                        "⭐".into()
                                    } else {
                                        "☰".into()
                                    };
                                }
                                KeyCode::Char('q') => break,
                                KeyCode::Char('/') => state.input_mode = InputMode::Editing,
                                KeyCode::Char('+') | KeyCode::Char('=') => {
                                    state.adjust_volume(0.05);
                                    state.last_action =
                                        format!("Vol: {:.0}%", state.volume * 100.0);
                                }
                                KeyCode::Char('-') | KeyCode::Char('_') => {
                                    state.adjust_volume(-0.05);
                                    state.last_action =
                                        format!("Vol: {:.0}%", state.volume * 100.0);
                                }
                                KeyCode::Up => {
                                    if key.modifiers.contains(event::KeyModifiers::ALT) {
                                        state.move_track(true);
                                        let tracks_copy = state.tracks.clone();
                                        tokio::spawn(async move {
                                            let _ = update_track_order(&tracks_copy).await;
                                        });
                                    } else {
                                        match state.active_pane {
                                            ActivePane::Menu => {
                                                let sel = state.menu_state.selected().unwrap_or(0);
                                                if sel > 0 {
                                                    state.menu_state.select(Some(sel - 1));
                                                    state.menu_selection =
                                                        MenuSelection::ALL[sel - 1];
                                                    state.apply_menu_selection();
                                                }
                                            }
                                            ActivePane::Content => {
                                                if state.content_current > 0 {
                                                    state.content_current -= 1;
                                                    state
                                                        .content_list_state
                                                        .select(Some(state.content_current));
                                                    if matches!(
                                                        state.content_view,
                                                        ContentView::TrackList
                                                            | ContentView::ArtistTracks(_)
                                                            | ContentView::AlbumTracks(_)
                                                            | ContentView::Favorites
                                                            | ContentView::PlaylistTracks(_)
                                                    ) {
                                                        state.current = state.content_current;
                                                        state
                                                            .list_state
                                                            .select(Some(state.current));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                KeyCode::Down => {
                                    if key.modifiers.contains(event::KeyModifiers::ALT) {
                                        state.move_track(false);
                                        let tracks_copy = state.tracks.clone();
                                        tokio::spawn(async move {
                                            let _ = update_track_order(&tracks_copy).await;
                                        });
                                    } else {
                                        match state.active_pane {
                                            ActivePane::Menu => {
                                                let sel = state.menu_state.selected().unwrap_or(0);
                                                if sel < MenuSelection::ALL.len() - 1 {
                                                    state.menu_state.select(Some(sel + 1));
                                                    state.menu_selection =
                                                        MenuSelection::ALL[sel + 1];
                                                    state.apply_menu_selection();
                                                }
                                            }
                                            ActivePane::Content => {
                                                let max =
                                                    state.content_item_count().saturating_sub(1);
                                                if state.content_current < max {
                                                    state.content_current += 1;
                                                    state
                                                        .content_list_state
                                                        .select(Some(state.content_current));
                                                    if matches!(
                                                        state.content_view,
                                                        ContentView::TrackList
                                                            | ContentView::ArtistTracks(_)
                                                            | ContentView::AlbumTracks(_)
                                                            | ContentView::Favorites
                                                            | ContentView::PlaylistTracks(_)
                                                    ) {
                                                        state.current = state.content_current;
                                                        state
                                                            .list_state
                                                            .select(Some(state.current));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                KeyCode::Left => {
                                    match state.active_pane {
                                        ActivePane::Menu => {
                                            // Nothing to do left of menu
                                        }
                                        ActivePane::Content => {
                                            let is_repeat = last_key.map_or(false, |(c, t)| {
                                                c == KeyCode::Left
                                                    && now.duration_since(t)
                                                        < Duration::from_millis(200)
                                            });
                                            if is_repeat {
                                                player::seek_relative(-5.0);
                                                state.last_action = "⏪".into();
                                            } else {
                                                // Go back up hierarchy or to menu
                                                match state.content_view {
                                                    ContentView::ArtistTracks(_) => {
                                                        state.content_view =
                                                            ContentView::ArtistList;
                                                        state.content_current = 0;
                                                        state.content_list_state.select(Some(0));
                                                    }
                                                    ContentView::AlbumTracks(_) => {
                                                        state.content_view = ContentView::AlbumList;
                                                        state.content_current = 0;
                                                        state.content_list_state.select(Some(0));
                                                    }
                                                    ContentView::PlaylistTracks(_) => {
                                                        state.content_view =
                                                            ContentView::PlaylistsList;
                                                        state.content_current = 0;
                                                        state.content_list_state.select(Some(0));
                                                    }
                                                    _ => {
                                                        state.active_pane = ActivePane::Menu;
                                                        state.last_action = "Menu Focus".into();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    last_key = Some((KeyCode::Left, now));
                                }
                                KeyCode::Right => {
                                    match state.active_pane {
                                        ActivePane::Menu => {
                                            state.active_pane = ActivePane::Content;
                                            state.last_action = "Content Focus".into();
                                        }
                                        ActivePane::Content => {
                                            let is_repeat = last_key.map_or(false, |(c, t)| {
                                                c == KeyCode::Right
                                                    && now.duration_since(t)
                                                        < Duration::from_millis(200)
                                            });
                                            if is_repeat {
                                                player::seek_relative(5.0);
                                                state.last_action = "⏩".into();
                                            } else if matches!(
                                                state.content_view,
                                                ContentView::TrackList
                                                    | ContentView::ArtistTracks(_)
                                                    | ContentView::AlbumTracks(_)
                                                    | ContentView::PlaylistTracks(_)
                                                    | ContentView::Favorites
                                            ) {
                                                if state.is_shuffle
                                                    && state.filtered_indices.len() > 1
                                                {
                                                    state.current = rand::thread_rng()
                                                        .gen_range(0..state.filtered_indices.len());
                                                    state.list_state.select(Some(state.current));
                                                    state.content_current = state.current;
                                                    state
                                                        .content_list_state
                                                        .select(Some(state.content_current));
                                                    state.last_action = "🔀".into();
                                                    play_selected_track(
                                                        &mut state,
                                                        tx_lyrics.clone(),
                                                        tx_player_status.clone(),
                                                        tx_album_art.clone(),
                                                        &mut video_task,
                                                    );
                                                } else if state.current
                                                    < state.filtered_indices.len().saturating_sub(1)
                                                {
                                                    state.current += 1;
                                                    state.list_state.select(Some(state.current));
                                                    state.content_current = state.current;
                                                    state
                                                        .content_list_state
                                                        .select(Some(state.content_current));
                                                    state.last_action = "⏭".into();
                                                    play_selected_track(
                                                        &mut state,
                                                        tx_lyrics.clone(),
                                                        tx_player_status.clone(),
                                                        tx_album_art.clone(),
                                                        &mut video_task,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    last_key = Some((KeyCode::Right, now));
                                }
                                KeyCode::Enter => {
                                    if state.is_playing_video {
                                        state.is_playing_video = false;
                                        state.video_rgb = None;
                                        state.video_cells = None;
                                        if let Some(task) = video_task.take() {
                                            task.abort();
                                        }
                                    }

                                    match state.active_pane {
                                        ActivePane::Menu => {
                                            state.active_pane = ActivePane::Content;
                                        }
                                        ActivePane::Content => match state.content_view {
                                            ContentView::ArtistList => {
                                                if state.content_current
                                                    < state.filtered_artist_list.len()
                                                {
                                                    let artist = state.filtered_artist_list
                                                        [state.content_current]
                                                        .clone();
                                                    state.filter_by_artist(&artist);
                                                }
                                            }
                                            ContentView::AlbumList => {
                                                if state.content_current
                                                    < state.filtered_album_list.len()
                                                {
                                                    let album = state.filtered_album_list
                                                        [state.content_current]
                                                        .clone();
                                                    state.filter_by_album(&album);
                                                }
                                            }
                                            ContentView::PlaylistsList => {
                                                let mut pl_names: Vec<&String> =
                                                    state.playlists.keys().collect();
                                                pl_names.sort();
                                                if state.content_current < pl_names.len() {
                                                    let pl =
                                                        pl_names[state.content_current].clone();
                                                    state.filter_by_playlist(&pl);
                                                }
                                            }
                                            ContentView::TrackList
                                            | ContentView::ArtistTracks(_)
                                            | ContentView::AlbumTracks(_)
                                            | ContentView::PlaylistTracks(_)
                                            | ContentView::Favorites => {
                                                state.last_action = "▶".into();
                                                play_selected_track(
                                                    &mut state,
                                                    tx_lyrics.clone(),
                                                    tx_player_status.clone(),
                                                    tx_album_art.clone(),
                                                    &mut video_task,
                                                );
                                            }
                                        },
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    if state.is_playing_video {
                                        state.is_playing_video = false;
                                        state.video_rgb = None;
                                        state.video_cells = None;
                                        if let Some(task) = video_task.take() {
                                            task.abort();
                                        }
                                    }

                                    if state.playing_id.is_some() {
                                        state.is_paused = player::toggle_pause();
                                        state.last_action = if state.is_paused {
                                            "⏸".into()
                                        } else {
                                            "▶".into()
                                        };
                                        publish_media(&mut state);
                                    }
                                }
                                KeyCode::Tab => {
                                    state.active_pane = match state.active_pane {
                                        ActivePane::Menu => ActivePane::Content,
                                        ActivePane::Content => ActivePane::Menu,
                                    };
                                }
                                KeyCode::Esc => {
                                    if matches!(state.active_pane, ActivePane::Content) {
                                        match state.content_view {
                                            ContentView::ArtistTracks(_) => {
                                                state.content_view = ContentView::ArtistList;
                                                state.content_current = 0;
                                                state.content_list_state.select(Some(0));
                                            }
                                            ContentView::AlbumTracks(_) => {
                                                state.content_view = ContentView::AlbumList;
                                                state.content_current = 0;
                                                state.content_list_state.select(Some(0));
                                            }
                                            ContentView::PlaylistTracks(_) => {
                                                state.content_view = ContentView::PlaylistsList;
                                                state.content_current = 0;
                                                state.content_list_state.select(Some(0));
                                            }
                                            _ => {
                                                state.active_pane = ActivePane::Menu;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            },
                            InputMode::PlaylistInput => match key.code {
                                KeyCode::Enter => {
                                    if !state.playlist_input.is_empty() {
                                        if let Some(track) = state.current_track() {
                                            let path = track.path.clone();
                                            state
                                                .playlists
                                                .entry(state.playlist_input.clone())
                                                .or_insert_with(Vec::new)
                                                .push(path);
                                            let _ = state.save_state();
                                            // Update UI if currently viewing PlaylistsList
                                            if state.content_view == ContentView::PlaylistsList {
                                                state.apply_menu_selection();
                                            }
                                        }
                                    }
                                    state.input_mode = InputMode::Normal;
                                    state.last_action = "Saved Playlist".into();
                                }
                                KeyCode::Char(c) => {
                                    state.playlist_input.push(c);
                                }
                                KeyCode::Backspace => {
                                    state.playlist_input.pop();
                                }
                                KeyCode::Esc => {
                                    state.input_mode = InputMode::Normal;
                                    state.last_action = "Cancel Playlist".into();
                                }
                                _ => {}
                            },
                            InputMode::Editing => match key.code {
                                KeyCode::Char(c) => {
                                    state.search.push(c);
                                    state.update_search();
                                }
                                KeyCode::Backspace => {
                                    state.search.pop();
                                    state.update_search();
                                }
                                KeyCode::Esc | KeyCode::Enter => {
                                    state.input_mode = InputMode::Normal
                                }
                                _ => {}
                            },
                        }
                    }
                    Event::Mouse(mouse) => {
                        let col = mouse.column;
                        let row = mouse.row;

                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // Check Buttons
                                if let Some(area) = state.prev_button_area {
                                    if col >= area.x && col < area.x + area.width && row == area.y {
                                        if state.current > 0 {
                                            state.current -= 1;
                                            state.list_state.select(Some(state.current));
                                            state.last_action = "⏮".into();
                                            play_selected_track(
                                                &mut state,
                                                tx_lyrics.clone(),
                                                tx_player_status.clone(),
                                                tx_album_art.clone(),
                                                &mut video_task,
                                            );
                                        }
                                    }
                                }
                                if let Some(area) = state.play_button_area {
                                    if col >= area.x && col < area.x + area.width && row == area.y {
                                        if state.playing_id.is_some() {
                                            state.is_paused = player::toggle_pause();
                                            state.last_action = if state.is_paused {
                                                "⏸".into()
                                            } else {
                                                "▶".into()
                                            };
                                            publish_media(&mut state);
                                        } else {
                                            state.last_action = "▶".into();
                                            play_selected_track(
                                                &mut state,
                                                tx_lyrics.clone(),
                                                tx_player_status.clone(),
                                                tx_album_art.clone(),
                                                &mut video_task,
                                            );
                                        }
                                    }
                                }
                                if let Some(area) = state.next_button_area {
                                    if col >= area.x && col < area.x + area.width && row == area.y {
                                        if state.is_shuffle && state.filtered_indices.len() > 1 {
                                            state.current = rand::thread_rng()
                                                .gen_range(0..state.filtered_indices.len());
                                            state.list_state.select(Some(state.current));
                                            state.last_action = "🔀".into();
                                            play_selected_track(
                                                &mut state,
                                                tx_lyrics.clone(),
                                                tx_player_status.clone(),
                                                tx_album_art.clone(),
                                                &mut video_task,
                                            );
                                        } else if state.current
                                            < state.filtered_indices.len().saturating_sub(1)
                                        {
                                            state.current += 1;
                                            state.list_state.select(Some(state.current));
                                            state.last_action = "⏭".into();
                                            play_selected_track(
                                                &mut state,
                                                tx_lyrics.clone(),
                                                tx_player_status.clone(),
                                                tx_album_art.clone(),
                                                &mut video_task,
                                            );
                                        }
                                    }
                                }

                                // Check Lyrics
                                if let Some(area) = state.lyric_area {
                                    if col >= area.x
                                        && col < area.x + area.width
                                        && row >= area.y
                                        && row < area.y + area.height
                                    {
                                        let relative_row = row as i32 - area.y as i32;
                                        let center_line = area.height as i32 / 2;
                                        // スクロールオフセットを考慮
                                        let line_offset =
                                            relative_row - center_line + state.lyric_scroll_offset;

                                        let pos = state.playback_pos;
                                        let mut current_idx = 0;
                                        for (i, (time, _)) in state.parsed_lyrics.iter().enumerate()
                                        {
                                            if pos >= *time {
                                                current_idx = i;
                                            } else {
                                                break;
                                            }
                                        }

                                        let target_idx = (current_idx as i32 + line_offset)
                                            .clamp(0, state.parsed_lyrics.len() as i32 - 1)
                                            as usize;
                                        let (target_time, _) = state.parsed_lyrics[target_idx];
                                        player::seek_to(target_time);
                                        state.last_action = format!("Seek: {:.0}s", target_time);
                                        state.lyric_scroll_offset = 0; // シークしたらリセット
                                    }
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                if let Some(area) = state.lyric_area {
                                    if col >= area.x
                                        && col < area.x + area.width
                                        && row >= area.y
                                        && row < area.y + area.height
                                    {
                                        state.lyric_scroll_offset -= 1;
                                        state.last_lyric_interaction = Instant::now();
                                    }
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                if let Some(area) = state.lyric_area {
                                    if col >= area.x
                                        && col < area.x + area.width
                                        && row >= area.y
                                        && row < area.y + area.height
                                    {
                                        state.lyric_scroll_offset += 1;
                                        state.last_lyric_interaction = Instant::now();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                } // match ev
            } // if event::poll
        } // if elapsed < FRAME_DURATION

        state.playback_pos = player::get_position();
        update_current_lyric(&mut state);

        if state.playing_id.is_some() && !state.is_paused && state.is_actually_playing {
            let reached_end = player::is_finished();
            let duration = state
                .tracks
                .iter()
                .find(|t| Some(&t.path) == state.playing_id.as_ref())
                .map(|t| t.duration)
                .unwrap_or(0.0);
            let is_near_end = state.playback_pos >= duration - 1.0 && duration > 0.0;

            if reached_end || is_near_end {
                let current_playing_idx = state
                    .filtered_indices
                    .iter()
                    .position(|&idx| Some(&state.tracks[idx].path) == state.playing_id.as_ref());
                if let Some(idx_in_filtered) = current_playing_idx {
                    if state.is_shuffle && state.filtered_indices.len() > 1 {
                        let mut next_idx =
                            rand::thread_rng().gen_range(0..state.filtered_indices.len());
                        if next_idx == idx_in_filtered {
                            next_idx = (next_idx + 1) % state.filtered_indices.len();
                        }
                        state.current = next_idx;
                        state.list_state.select(Some(state.current));
                        state.last_action = "🔀".into();
                        play_selected_track(
                            &mut state,
                            tx_lyrics.clone(),
                            tx_player_status.clone(),
                            tx_album_art.clone(),
                            &mut video_task,
                        );
                    } else if idx_in_filtered < state.filtered_indices.len() - 1 {
                        state.current = idx_in_filtered + 1;
                        state.list_state.select(Some(state.current));
                        state.last_action = "⏭".into();
                        play_selected_track(
                            &mut state,
                            tx_lyrics.clone(),
                            tx_player_status.clone(),
                            tx_album_art.clone(),
                            &mut video_task,
                        );
                    }
                }
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(100) {
            state.tick_count += 1;
            last_tick = Instant::now();
        }
    }
    restore_terminal();
    Ok(())
}

fn play_selected_track(
    state: &mut AppState,
    tx_lyrics: tokio::sync::mpsc::Sender<(String, Result<String>)>,
    tx_status: tokio::sync::mpsc::Sender<String>,
    tx_art: tokio::sync::mpsc::Sender<Vec<u8>>,
    video_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if state.is_playing_video {
        state.is_playing_video = false;
        state.video_rgb = None;
        state.video_cells = None;
        if let Some(task) = video_task.take() {
            task.abort();
        }
    }

    let (path, lrc_path, title, artist, album) = if let Some(t) = state.current_track() {
        (
            t.path.clone(),
            t.lrc.clone(),
            t.title.clone(),
            t.artist.clone(),
            t.album.clone(),
        )
    } else {
        return;
    };

    state.error_msg = None;
    state.fetch_paused.store(true, Ordering::SeqCst);
    state.playing_id = Some(path.clone());
    state.status_msg = "Starting...".into();
    state.current_lyric = "Buffering...".into();
    state.parsed_lyrics.clear();
    state.is_paused = false;
    state.is_actually_playing = false;
    state.album_art = None;
    state.art_temp_path = None;
    state.lyric_scroll_offset = 0;

    // Update system metadata(前の曲の歌詞ファイルは消す。新しい歌詞が届いたら書き直す)
    write_lyrics_sidecar(&title, &artist, &album, None);
    publish_media(state);

    let url = stream_url_from_path(&path);
    let _ = player::play_from_url_streaming(url, tx_status, tx_art);

    if let Some(lp) = lrc_path {
        let tx = tx_lyrics.clone();
        let path_copy = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let res = fetch_lyrics_from_url(&lyrics_url_from_path(&lp)).await;
            let _ = tx.send((path_copy, res)).await;
        });
    }

    let pause_signal = state.fetch_paused.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        pause_signal.store(false, Ordering::SeqCst);
    });
}

fn update_current_lyric(state: &mut AppState) {
    if state.parsed_lyrics.is_empty() {
        return;
    }
    let mut line = "";
    for (time, text) in &state.parsed_lyrics {
        if state.playback_pos >= *time {
            line = text;
        } else {
            break;
        }
    }
    state.current_lyric = line.to_string();
}

fn parse_lrc(lrc: &str) -> Vec<(f64, String)> {
    let mut result = Vec::new();
    for line in lrc.lines() {
        if let Some(pos) = line.find(']') {
            let time_str = &line[1..pos];
            let parts: Vec<&str> = time_str.split(':').collect();
            if parts.len() == 2 {
                let m: f64 = parts[0].parse().unwrap_or(0.0);
                let s: f64 = parts[1].parse().unwrap_or(0.0);
                result.push((m * 60.0 + s, line[pos + 1..].trim().to_string()));
            }
        }
    }
    result.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    result
}

fn spawn_video_task(
    url: String,
    start_pos: f64,
    tx: tokio::sync::mpsc::Sender<(Vec<u8>, u32, u32)>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        // === Audio: ffmpeg decodes to raw PCM, played through rodio (same output as music) ===
        // (ffmpeg's own ALSA output is missing in some builds, e.g. Chimera's ffmpeg has only pulse)
        let audio_url = url.clone();
        let audio = async move {
            let Ok(out) = player::VideoAudio::new() else {
                return;
            };
            let rate = player::VIDEO_AUDIO_RATE.to_string();
            let channels = player::VIDEO_AUDIO_CHANNELS.to_string();
            let Ok(mut child) = tokio::process::Command::new("ffmpeg")
                .kill_on_drop(true)
                .args(&[
                    "-reconnect",
                    "1",
                    "-reconnect_streamed",
                    "1",
                    "-reconnect_delay_max",
                    "2",
                    "-ss",
                    &format!("{:.2}", start_pos),
                    "-i",
                    &audio_url,
                    "-vn",
                    "-f",
                    "s16le",
                    "-acodec",
                    "pcm_s16le",
                    "-ac",
                    &channels,
                    "-ar",
                    &rate,
                    "-",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                return;
            };
            let Some(mut pcm) = child.stdout.take() else {
                return;
            };
            // ~85ms chunks; keep at most ~0.7s queued so playback follows the video clock
            let mut buf = vec![0u8; 4096 * 4];
            loop {
                while out.queued() > 8 {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                let mut filled = 0;
                while filled < buf.len() {
                    match pcm.read(&mut buf[filled..]).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => filled += n,
                    }
                }
                let whole = filled - filled % 4;
                if whole > 0 {
                    out.push(&buf[..whole]);
                }
                if filled < buf.len() {
                    break;
                }
            }
            // let the queued tail finish before `out` is dropped (dropping stops it)
            while out.queued() > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };

        // === Video process (pipe raw RGB to stdout) ===
        let video = async move {
            let mut video_child = match tokio::process::Command::new("ffmpeg")
                .kill_on_drop(true)
                .args(&[
                    "-re",
                    "-reconnect",
                    "1",
                    "-reconnect_streamed",
                    "1",
                    "-reconnect_delay_max",
                    "2",
                    "-ss",
                    &format!("{:.2}", start_pos),
                    "-i",
                    &url,
                    "-an",
                    "-vf",
                    "scale=320:180:flags=fast_bilinear",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "-r",
                    "15",
                    "-",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => return,
            };

            let mut stdout = video_child.stdout.take().unwrap();
            let frame_size = 320 * 180 * 3;

            // Send raw RGB frames; bounded channel drops stale frames
            loop {
                let mut frame_buf = vec![0u8; frame_size];
                if stdout.read_exact(&mut frame_buf).await.is_err() {
                    break;
                }
                match tx.try_send((frame_buf, 320, 180)) {
                    Ok(_) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        };

        // Both run inside this task: aborting it drops the ffmpeg children and stops the audio sink
        tokio::join!(audio, video);
    })
}

/// OS のメディア連携(MPRIS)に、今の曲の情報(アルバム・長さ・ジャケット)と再生位置を送る。
/// ame-shell などのダッシュボードは、これでアルバムアート・進み具合・同期歌詞を表示する。
/// (再生位置は送った時点の値しか伝わらないので、再生中は 1 秒ごとに呼ぶ)
fn publish_media(state: &mut AppState) {
    let Some(id) = state.playing_id.clone() else {
        return;
    };
    let (title, artist, album, duration) = state
        .tracks
        .iter()
        .find(|t| t.path == id)
        .map(|t| {
            (
                t.title.clone(),
                t.artist.clone(),
                t.album.clone(),
                t.duration,
            )
        })
        .unwrap_or_default();
    let art_url = state
        .art_temp_path
        .as_ref()
        .map(|p| format!("file://{}", p));
    let paused = state.is_paused;
    let pos = player::get_position();
    if let Some(controls) = &mut state.media_controls {
        // 曲の情報は変わったときだけ送る(毎秒送ると、受け取る側で曲が替わったように扱われることがある)
        static LAST_META: once_cell::sync::Lazy<std::sync::Mutex<String>> =
            once_cell::sync::Lazy::new(|| std::sync::Mutex::new(String::new()));
        let key = format!(
            "{title}\u{1f}{artist}\u{1f}{album}\u{1f}{:?}\u{1f}{duration}",
            art_url
        );
        let changed = LAST_META
            .lock()
            .map(|mut last| {
                if *last == key {
                    false
                } else {
                    *last = key;
                    true
                }
            })
            .unwrap_or(true);
        if changed {
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(&title),
                artist: Some(&artist),
                album: Some(&album),
                cover_url: art_url.as_deref(),
                duration: if duration > 0.0 {
                    Some(Duration::from_secs_f64(duration))
                } else {
                    None
                },
            });
        }
        let progress = Some(souvlaki::MediaPosition(Duration::from_secs_f64(
            pos.max(0.0),
        )));
        let _ = controls.set_playback(if paused {
            MediaPlayback::Paused { progress }
        } else {
            MediaPlayback::Playing { progress }
        });
    }
}

/// 曲ごとに別のジャケットのファイル名(同じ名前だと、表示側が「変わっていない」と見なして前の曲の絵のままになる)
fn art_path_for(track_path: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    track_path.hash(&mut h);
    std::env::temp_dir().join(format!("music_tui_art_{:016x}.png", h.finish()))
}

/// 同期歌詞を $XDG_RUNTIME_DIR/lyrics/music_tui.lrc に書く(ame-shell のダッシュボードが、
/// ネットで探す代わりにこの曲そのものの歌詞を使う)。[ti:] [ar:] [al:] の LRC の標準タグで、どの曲のものかを示す
fn write_lyrics_sidecar(title: &str, artist: &str, album: &str, lrc: Option<&str>) {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("lyrics");
    let path = dir.join("music_tui.lrc");
    match lrc {
        Some(text) => {
            let _ = std::fs::create_dir_all(&dir);
            let body = format!("[ti:{}]\n[ar:{}]\n[al:{}]\n{}", title, artist, album, text);
            // 書きかけを読まれないよう、別名で書いてから置き換える
            let tmp = dir.join("music_tui.lrc.tmp");
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}
