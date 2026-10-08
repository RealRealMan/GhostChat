//! Visual effects. Every effect draws public or random data only — never key
//! material (PRD §12). The only real data shown is a peer's PUBLIC key.

use std::time::Duration;

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Stroke, Vec2};

use super::theme::{Palette, mono};

/// CRT scanlines over `rect`.
pub fn scanlines(painter: &egui::Painter, rect: Rect) {
    let stroke = Stroke::new(1.0, Color32::from_black_alpha(70));
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.hline(rect.x_range(), y, stroke);
        y += 3.0;
    }
}

/// Slight brightness flicker.
pub fn flicker(painter: &egui::Painter, rect: Rect, t: f64) {
    let a = ((t * 13.0).sin() * 0.5 + 0.5) * 10.0;
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(a as u8));
}

/// Random horizontal tears and noise; `intensity` in 0..=1.
pub fn glitch(painter: &egui::Painter, rect: Rect, p: &Palette, intensity: f32) {
    let bands = (intensity * 18.0) as usize + 2;
    for _ in 0..bands {
        let y = rect.top() + rand::random::<f32>() * rect.height();
        let h = 2.0 + rand::random::<f32>() * 14.0 * intensity;
        let x = rect.left() + (rand::random::<f32>() - 0.3) * 60.0 * intensity;
        let w = rect.width() * (0.2 + rand::random::<f32>() * 0.8);
        let color = match rand::random::<u8>() % 3 {
            0 => p.danger,
            1 => p.fg,
            _ => p.bright,
        }
        .gamma_multiply(0.25 + 0.5 * intensity);
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)),
            0.0,
            color,
        );
    }
    let chars = (intensity * 40.0) as usize;
    for _ in 0..chars {
        let pos = Pos2::new(
            rect.left() + rand::random::<f32>() * rect.width(),
            rect.top() + rand::random::<f32>() * rect.height(),
        );
        painter.text(
            pos,
            Align2::LEFT_TOP,
            format!("{:02X}", rand::random::<u8>()),
            mono(14.0),
            p.danger.gamma_multiply(0.8),
        );
    }
}

/// The first `cps * elapsed` characters of `text` (typewriter effect).
pub fn typewriter(text: &str, elapsed: Duration, cps: f32) -> &str {
    let n = (elapsed.as_secs_f32() * cps) as usize;
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

pub fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Matrix-style rain for the idle screen. Random characters only.
#[derive(Default)]
pub struct Rain {
    drops: Vec<f32>,
    speeds: Vec<f32>,
}

const RAIN_CHARS: &[u8] = b"0123456789ABCDEF<>/\\|=+*#@$%&";
const CELL: f32 = 16.0;

impl Rain {
    pub fn draw(&mut self, painter: &egui::Painter, rect: Rect, p: &Palette, dt: f32) {
        painter.rect_filled(rect, 0.0, p.bg);
        let cols = (rect.width() / CELL).ceil() as usize;
        if self.drops.len() != cols {
            self.drops = (0..cols)
                .map(|_| rand::random::<f32>() * rect.height())
                .collect();
            self.speeds = (0..cols)
                .map(|_| 80.0 + rand::random::<f32>() * 220.0)
                .collect();
        }
        let trail = 14;
        for (i, (y, speed)) in self.drops.iter_mut().zip(&self.speeds).enumerate() {
            *y += speed * dt;
            if *y - trail as f32 * CELL > rect.height() {
                *y = -rand::random::<f32>() * rect.height() * 0.5;
            }
            let x = rect.left() + i as f32 * CELL;
            for k in 0..trail {
                let cy = rect.top() + *y - k as f32 * CELL;
                if cy < rect.top() - CELL || cy > rect.bottom() {
                    continue;
                }
                let c = RAIN_CHARS[rand::random::<u8>() as usize % RAIN_CHARS.len()] as char;
                let color = if k == 0 {
                    p.bright
                } else {
                    p.fg.gamma_multiply(1.0 - k as f32 / trail as f32)
                };
                painter.text(
                    Pos2::new(x, cy),
                    Align2::LEFT_TOP,
                    c,
                    mono(CELL - 2.0),
                    color,
                );
            }
        }
    }
}

pub const HANDSHAKE: Duration = Duration::from_millis(2200);

/// Handshake visualization: protocol steps, scrolling hex (random bytes and
/// the peer's PUBLIC key), then SECURE CHANNEL ESTABLISHED.
pub fn handshake(
    painter: &egui::Painter,
    rect: Rect,
    p: &Palette,
    elapsed: Duration,
    peer_name: &str,
    peer_public_hex: &str,
    path: &str,
) {
    let steps = [
        format!("> locating {peer_name} ..."),
        format!("> route: {path}"),
        "> QUIC + TLS 1.3 handshake (Ed25519 raw public keys)".to_string(),
        format!("> peer public key {peer_public_hex}"),
        "> allowlist check ........ PASS".to_string(),
    ];
    let t = elapsed.as_secs_f32();
    let box_rect = Rect::from_min_size(
        rect.left_top() + Vec2::new(12.0, 12.0),
        Vec2::new(rect.width().min(760.0) - 24.0, 200.0),
    );
    painter.rect_filled(box_rect, 4.0, p.panel.gamma_multiply(0.97));
    painter.rect_stroke(
        box_rect,
        4.0,
        Stroke::new(1.0, p.fg),
        egui::StrokeKind::Inside,
    );
    let mut y = box_rect.top() + 10.0;
    let shown = ((t / 0.3) as usize).min(steps.len());
    for s in &steps[..shown] {
        painter.text(
            Pos2::new(box_rect.left() + 10.0, y),
            Align2::LEFT_TOP,
            s,
            mono(13.0),
            p.fg,
        );
        y += 18.0;
    }
    if t < 1.6 {
        for _ in 0..3 {
            painter.text(
                Pos2::new(box_rect.left() + 10.0, y),
                Align2::LEFT_TOP,
                random_hex(24),
                mono(12.0),
                p.dim,
            );
            y += 15.0;
        }
    } else {
        painter.text(
            Pos2::new(box_rect.left() + 10.0, y + 8.0),
            Align2::LEFT_TOP,
            "SECURE CHANNEL ESTABLISHED",
            mono(22.0),
            p.bright,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typewriter_reveals_progressively() {
        assert_eq!(typewriter("HELLO", Duration::ZERO, 10.0), "");
        assert_eq!(typewriter("HELLO", Duration::from_millis(250), 10.0), "HE");
        assert_eq!(typewriter("HELLO", Duration::from_secs(5), 10.0), "HELLO");
        assert_eq!(
            typewriter("你好世界", Duration::from_millis(200), 10.0),
            "你好"
        );
    }
}
