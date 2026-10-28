use crate::api::TrackInfo;
use image::DynamicImage;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use souvlaki::{MediaControlEvent, MediaControls};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Instant;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Editing,
    PlaylistInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Menu,
    Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSelection {
    AllTracks,
    Artists,
    Albums,
    Favorites,
    Playlists,
}

impl MenuSelection {
    pub const ALL: [MenuSelection; 5] = [
        MenuSelection::AllTracks,
        MenuSelection::Artists,
        MenuSelection::Albums,
        MenuSelection::Favorites,
        MenuSelection::Playlists,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            MenuSelection::AllTracks => "♫ ALL SONGS",
            MenuSelection::Artists => " ARTISTS",
            MenuSelection::Albums => " ALBUMS",
            MenuSelection::Favorites => " FAVORITES",
            MenuSelection::Playlists => " PLAYLISTS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentView {
    TrackList,
    ArtistList,
    AlbumList,
    Favorites,
    PlaylistsList,
    ArtistTracks(String),
    AlbumTracks(String),
    PlaylistTracks(String),
}

const FAV_FILE: &str = "favorites.json";
const PLAYLISTS_FILE: &str = "playlists.json";

pub struct AppState {
    pub tracks: Vec<TrackInfo>,
    pub filtered_indices: Vec<usize>,
    pub current: usize,
    pub list_state: ListState,
    pub input_mode: InputMode,
    pub current_lyric: String,
    pub parsed_lyrics: Vec<(f64, String)>,
    pub lyric_area: Option<Rect>,
    pub video_area: Option<Rect>,
    pub seek_bar_area: Option<Rect>,
    pub lyric_scroll_offset: i32,
    pub last_lyric_interaction: Instant,
    pub prev_button_area: Option<Rect>,
    pub play_button_area: Option<Rect>,
    pub next_button_area: Option<Rect>,
    pub search: String,
    pub is_loading: bool,
    pub load_progress: f64,
    pub fetch_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub error_msg: Option<String>,
    pub status_msg: String,
    pub last_action: String,
    pub tick_count: u64,
    pub playback_pos: f64,
    pub playing_id: Option<String>,
    pub is_paused: bool,
    pub is_actually_playing: bool,
    pub favorites: HashSet<String>,
    pub show_favorites_only: bool,
    pub volume: f32,
    pub is_playing_video: bool,
    pub art_temp_path: Option<String>,
    pub album_art: Option<DynamicImage>,
    /// キャッシュ済みアルバムアートprotocol (毎フレーム再生成を避けるため)
    pub album_art_protocol: Option<Protocol>,
    pub picker: Option<Picker>,
    // 動画フレーム: Raw RGB + サイズ + 描画済みセルバッファ
    pub video_rgb: Option<(Vec<u8>, u32, u32)>,
    pub video_cells: Option<Vec<u8>>,
    pub video_cell_area: Option<Rect>,
    pub video_frame_dirty: bool,
    // Media controls for OS integration
    pub media_controls: Option<MediaControls>,
    pub rx_media_events: mpsc::Receiver<MediaControlEvent>,
    pub video_playback_pos: f64,
    pub video_duration: f64,
    // --- Menu / Content pane state ---
    pub active_pane: ActivePane,
    pub menu_selection: MenuSelection,
    pub menu_state: ListState,
    pub content_view: ContentView,
    pub content_list_state: ListState,
    pub content_current: usize,
    /// Cached sorted unique artist names
    pub artist_list: Vec<String>,
    /// Cached sorted unique album names
    pub album_list: Vec<String>,
    /// Search-filtered artist list (used in ArtistList view)
    pub filtered_artist_list: Vec<String>,
    /// Search-filtered album list (used in AlbumList view)
    pub filtered_album_list: Vec<String>,
    pub is_shuffle: bool,
    pub playlists: HashMap<String, Vec<String>>,
    pub playlist_input: String,
}

impl AppState {
    pub fn new(tracks: Vec<TrackInfo>) -> Self {
        let favorites = Self::load_favorites().unwrap_or_default();
        let filtered_indices = (0..tracks.len()).collect();
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let picker = Picker::from_query_stdio().ok();
        let (tx, rx) = mpsc::channel(32);

        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        let media_controls = {
            use souvlaki::PlatformConfig;
            let config = PlatformConfig {
                dbus_name: "org.mpris.MediaPlayer2.music_tui",
                display_name: "Music TUI",
                hwnd: None,
            };
            if let Ok(mut mc) = MediaControls::new(config) {
                let tx_clone = tx.clone();
                let _ = mc.attach(move |event| {
                    let _ = tx_clone.blocking_send(event);
                });
                Some(mc)
            } else {
                None
            }
        };
        #[cfg(not(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        )))]
        let media_controls = None;

        let mut menu_state = ListState::default();
        menu_state.select(Some(0));
        let mut content_list_state = ListState::default();
        content_list_state.select(Some(0));
        let playlists = Self::load_playlists().unwrap_or_default();

        Self {
            tracks,
            filtered_indices,
            current: 0,
            list_state,
            input_mode: InputMode::Normal,
            current_lyric: "".into(),
            parsed_lyrics: vec![],
            lyric_area: None,
            video_area: None,
            seek_bar_area: None,
            lyric_scroll_offset: 0,
            last_lyric_interaction: Instant::now(),
            prev_button_area: None,
            play_button_area: None,
            next_button_area: None,
            search: "".into(),
            is_loading: true,
            load_progress: 0.0,
            fetch_paused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            error_msg: None,
            status_msg: "Waiting for tracks...".into(),
            last_action: "■".into(),
            tick_count: 0,
            playback_pos: 0.0,
            playing_id: None,
            is_paused: false,
            is_actually_playing: false,
            favorites,
            show_favorites_only: false,
            volume: 1.0,
            is_playing_video: false,
            art_temp_path: None,
            album_art: None,
            album_art_protocol: None,
            picker,
            video_rgb: None,
            video_cells: None,
            video_cell_area: None,
            video_frame_dirty: false,
            media_controls,
            rx_media_events: rx,
            video_playback_pos: 0.0,
            video_duration: 0.0,
            active_pane: ActivePane::Menu,
            menu_selection: MenuSelection::AllTracks,
            menu_state,
            content_view: ContentView::TrackList,
            content_list_state,
            content_current: 0,
            artist_list: vec![],
            album_list: vec![],
            filtered_artist_list: vec![],
            filtered_album_list: vec![],
            is_shuffle: false,
            playlists,
            playlist_input: String::new(),
        }
    }

    fn load_favorites() -> Option<HashSet<String>> {
        if let Ok(data) = fs::read_to_string(FAV_FILE) {
            serde_json::from_str(&data).ok()
        } else {
            None
        }
    }

    pub fn save_state(&self) -> Result<(), anyhow::Error> {
        let data = serde_json::to_string(&self.favorites)?;
        fs::write(FAV_FILE, data)?;

        let pl_data = serde_json::to_string(&self.playlists)?;
        fs::write(PLAYLISTS_FILE, pl_data)?;
        Ok(())
    }

    fn load_playlists() -> Option<HashMap<String, Vec<String>>> {
        if let Ok(data) = fs::read_to_string(PLAYLISTS_FILE) {
            serde_json::from_str(&data).ok()
        } else {
            None
        }
    }

    pub fn current_track(&self) -> Option<&TrackInfo> {
        if self.tracks.is_empty() || self.filtered_indices.is_empty() {
            return None;
        }
        let idx = if self.current >= self.filtered_indices.len() {
            self.filtered_indices[0]
        } else {
            self.filtered_indices[self.current]
        };
        Some(&self.tracks[idx])
    }

    pub fn update_search(&mut self) {
        let search_lower = self.search.to_lowercase();

        let base_indices: Vec<usize> = match &self.content_view {
            ContentView::ArtistTracks(artist) => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.artist == *artist)
                .map(|(i, _)| i)
                .collect(),
            ContentView::AlbumTracks(album) => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.album == *album)
                .map(|(i, _)| i)
                .collect(),
            ContentView::PlaylistTracks(name) => {
                if let Some(paths) = self.playlists.get(name) {
                    let path_set: HashSet<_> = paths.iter().collect();
                    self.tracks
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| path_set.contains(&t.path))
                        .map(|(i, _)| i)
                        .collect()
                } else {
                    (0..self.tracks.len()).collect()
                }
            }
            ContentView::Favorites => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| self.favorites.contains(&t.path))
                .map(|(i, _)| i)
                .collect(),
            ContentView::TrackList if self.show_favorites_only => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| self.favorites.contains(&t.path))
                .map(|(i, _)| i)
                .collect(),
            _ => (0..self.tracks.len()).collect(),
        };

        self.filtered_indices = if search_lower.is_empty() {
            base_indices
        } else {
            base_indices
                .into_iter()
                .filter(|&i| {
                    let t = &self.tracks[i];
                    t.title.to_lowercase().contains(&search_lower)
                        || t.artist.to_lowercase().contains(&search_lower)
                        || t.album.to_lowercase().contains(&search_lower)
                })
                .collect()
        };

        if self.current >= self.filtered_indices.len() {
            self.current = if self.filtered_indices.is_empty() {
                0
            } else {
                self.filtered_indices.len() - 1
            };
        }
        let content_max = match &self.content_view {
            ContentView::ArtistList => self.filtered_artist_list.len(),
            ContentView::AlbumList => self.filtered_album_list.len(),
            ContentView::PlaylistsList => self.playlists.len(),
            _ => self.filtered_indices.len(),
        };
        if self.content_current >= content_max {
            self.content_current = if content_max == 0 { 0 } else { content_max - 1 };
        }
        self.list_state.select(Some(self.current));
        self.content_list_state.select(Some(self.content_current));

        if search_lower.is_empty() {
            self.filtered_artist_list = self.artist_list.clone();
            self.filtered_album_list = self.album_list.clone();
        } else {
            self.filtered_artist_list = self
                .artist_list
                .iter()
                .filter(|a| a.to_lowercase().contains(&search_lower))
                .cloned()
                .collect();
            self.filtered_album_list = self
                .album_list
                .iter()
                .filter(|a| a.to_lowercase().contains(&search_lower))
                .cloned()
                .collect();
        }
    }

    pub fn toggle_favorite(&mut self) {
        if let Some(track) = self.current_track() {
            let path = track.path.clone();
            if self.favorites.contains(&path) {
                self.favorites.remove(&path);
            } else {
                self.favorites.insert(path);
            }
            let _ = self.save_state();
            if self.show_favorites_only {
                self.update_search();
            }
        }
    }

    pub fn toggle_favorite_view(&mut self) {
        self.show_favorites_only = !self.show_favorites_only;
        self.update_search();
    }

    pub fn adjust_volume(&mut self, delta: f32) {
        self.volume = (self.volume + delta).clamp(0.0, 1.0);
        crate::player::set_volume(self.volume);
    }

    pub fn move_track(&mut self, up: bool) {
        if self.filtered_indices.len() < 2 {
            return;
        }
        if self.current >= self.filtered_indices.len() {
            return;
        }
        let target_idx = if up {
            if self.current == 0 {
                return;
            }
            self.current - 1
        } else {
            if self.current >= self.filtered_indices.len() - 1 {
                return;
            }
            self.current + 1
        };
        let actual_idx = self.filtered_indices[self.current];
        let actual_target_idx = self.filtered_indices[target_idx];
        self.tracks.swap(actual_idx, actual_target_idx);
        self.filtered_indices[self.current] = actual_target_idx;
        self.filtered_indices[target_idx] = actual_idx;
        self.current = target_idx;
        self.list_state.select(Some(self.current));
        self.last_action = format!("Moved {}", if up { "Up" } else { "Down" });
    }

    /// Rebuild the cached artist and album lists from tracks
    pub fn finalize_loading(&mut self) {
        self.tracks.sort_by(|a, b| {
            a.artist
                .to_lowercase()
                .cmp(&b.artist.to_lowercase())
                .then_with(|| a.album.to_lowercase().cmp(&b.album.to_lowercase()))
                .then_with(|| a.track_number.cmp(&b.track_number))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
        self.rebuild_caches();
    }

    pub fn rebuild_caches(&mut self) {
        let mut artists: Vec<String> = self
            .tracks
            .iter()
            .map(|t| t.artist.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        artists.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        self.artist_list = artists;

        // Build album list
        let mut albums: HashSet<(String, String)> = HashSet::new();
        for track in &self.tracks {
            if !track.album.is_empty() {
                albums.insert((track.artist.clone(), track.album.clone()));
            }
        }
        let mut album_vec: Vec<(String, String)> = albums.into_iter().collect();
        // Sort by artist, then by album
        album_vec.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        self.album_list = album_vec.into_iter().map(|(_, album)| album).collect();
    }

    /// Called when the user switches menu selection; updates content_view and filtered_indices
    pub fn apply_menu_selection(&mut self) {
        self.content_current = 0;
        self.content_list_state.select(Some(0));
        match self.menu_selection {
            MenuSelection::AllTracks => {
                self.show_favorites_only = false;
                self.content_view = ContentView::TrackList;
                self.update_search();
            }
            MenuSelection::Artists => {
                self.rebuild_caches();
                self.content_view = ContentView::ArtistList;
                self.update_search();
            }
            MenuSelection::Albums => {
                self.rebuild_caches();
                self.content_view = ContentView::AlbumList;
                self.update_search();
            }
            MenuSelection::Favorites => {
                self.show_favorites_only = true;
                self.content_view = ContentView::Favorites;
                self.update_search();
            }
            MenuSelection::Playlists => {
                self.show_favorites_only = false;
                self.content_view = ContentView::PlaylistsList;
            }
        }
    }

    /// Filter tracks for a specific artist or album
    pub fn filter_by_artist(&mut self, artist: &str) {
        let search_lower = self.search.to_lowercase();
        self.filtered_indices = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let matches_artist = t.artist == artist;
                let matches_search = search_lower.is_empty()
                    || t.title.to_lowercase().contains(&search_lower)
                    || t.artist.to_lowercase().contains(&search_lower);
                matches_artist && matches_search
            })
            .map(|(i, _)| i)
            .collect();
        self.current = 0;
        self.list_state.select(Some(0));
        self.content_current = 0;
        self.content_list_state.select(Some(0));
        self.content_view = ContentView::ArtistTracks(artist.to_string());
    }

    pub fn filter_by_album(&mut self, album: &str) {
        let search_lower = self.search.to_lowercase();
        self.filtered_indices = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let matches_album = t.album == album;
                let matches_search = search_lower.is_empty()
                    || t.title.to_lowercase().contains(&search_lower)
                    || t.album.to_lowercase().contains(&search_lower);
                matches_album && matches_search
            })
            .map(|(i, _)| i)
            .collect();
        self.current = 0;
        self.list_state.select(Some(0));
        self.content_current = 0;
        self.content_list_state.select(Some(0));
        self.content_view = ContentView::AlbumTracks(album.to_string());
    }

    /// Get the count of content items for the current view
    pub fn content_item_count(&self) -> usize {
        match &self.content_view {
            ContentView::TrackList => self.filtered_indices.len(),
            ContentView::ArtistList => self.filtered_artist_list.len(),
            ContentView::AlbumList => self.filtered_album_list.len(),
            ContentView::PlaylistsList => self.playlists.len(),
            ContentView::ArtistTracks(_)
            | ContentView::AlbumTracks(_)
            | ContentView::PlaylistTracks(_)
            | ContentView::Favorites => self.filtered_indices.len(),
        }
    }

    pub fn filter_by_playlist(&mut self, playlist: &str) {
        if let Some(paths) = self.playlists.get(playlist) {
            let path_set: HashSet<_> = paths.iter().collect();
            self.filtered_indices = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| path_set.contains(&t.path))
                .map(|(i, _)| i)
                .collect();
            self.content_view = ContentView::PlaylistTracks(playlist.to_string());
        }
    }
}
