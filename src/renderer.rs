use ratatui::{prelude::*, widgets::Paragraph};

/// Braille cell byte layout: [utf8_0..3, R, G, B, _]
const BRAILLE_BYTES: usize = 8;

fn le_bytes_to_char(bytes: &[u8]) -> char {
    let cp = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    char::from_u32(cp).unwrap_or(' ')
}

/// Convert pre-rendered braille cells to ratatui Lines.
pub fn braille_cells_to_lines(
    cells: &[u8],
    cell_width: u32,
    cell_height: u32,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(cell_height as usize);
    for cy in 0..cell_height {
        let mut spans: Vec<Span> = Vec::with_capacity(cell_width as usize);
        for cx in 0..cell_width {
            let idx = ((cy * cell_width + cx) * BRAILLE_BYTES as u32) as usize;
            if idx + BRAILLE_BYTES > cells.len() {
                break;
            }
            let ch = le_bytes_to_char(&cells[idx..idx + 4]);
            let r = cells[idx + 4];
            let g = cells[idx + 5];
            let b = cells[idx + 6];

            if ch != '\0' && ch != ' ' {
                spans.push(Span::styled(
                    ch.to_string(),
                    Style::default().fg(Color::Rgb(r, g, b)),
                ));
            } else {
                spans.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Render a pre-rendered braille cell buffer into a ratatui area.
pub fn render_braille_cells(frame: &mut Frame, cells: &[u8], cell_w: u32, cell_h: u32, area: Rect) {
    if cells.is_empty() || cell_w == 0 || cell_h == 0 {
        return;
    }
    let lines = braille_cells_to_lines(cells, cell_w, cell_h);
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render raw RGB pixels to braille cells.
pub fn render_braille(pixels: &[u8], w: u32, h: u32, cell_w: u32, cell_h: u32) -> Option<Vec<u8>> {
    terminal_pixel_animation::render_braille(pixels, w, h, cell_w, cell_h).ok()
}
