use eframe::egui;

const SIZE: u32 = 64;

pub fn load() -> egui::IconData {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    let top: [u8; 4] = [0xE8, 0xE2, 0xD2, 0xFF];
    let right: [u8; 4] = [0x9A, 0x8F, 0x76, 0xFF];
    let left: [u8; 4] = [0x6E, 0x63, 0x4E, 0xFF];
    let outline: [u8; 4] = [0x1A, 0x17, 0x10, 0xFF];

    let t_top = (32, 6);
    let t_right = (58, 19);
    let t_bot = (32, 32);
    let t_left = (6, 19);
    let b_right = (58, 45);
    let b_bot = (32, 58);
    let b_left = (6, 45);

    fill_quad(&mut rgba, t_top, t_right, t_bot, t_left, top);
    fill_quad(&mut rgba, t_bot, t_right, b_right, b_bot, right);
    fill_quad(&mut rgba, t_left, t_bot, b_bot, b_left, left);

    for (a, b) in [
        (t_top, t_right),
        (t_right, t_bot),
        (t_bot, t_left),
        (t_left, t_top),
        (t_right, b_right),
        (b_right, b_bot),
        (b_bot, b_left),
        (b_left, t_left),
        (t_bot, b_bot),
    ] {
        draw_line(&mut rgba, a, b, outline);
    }

    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

type P = (i32, i32);

fn put(rgba: &mut [u8], x: i32, y: i32, c: [u8; 4]) {
    if x < 0 || y < 0 || x >= SIZE as i32 || y >= SIZE as i32 {
        return;
    }
    let idx = ((y as u32 * SIZE + x as u32) * 4) as usize;
    rgba[idx..idx + 4].copy_from_slice(&c);
}

fn edge(a: P, b: P, p: P) -> i32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

fn fill_tri(rgba: &mut [u8], p0: P, p1: P, p2: P, c: [u8; 4]) {
    let min_x = p0.0.min(p1.0).min(p2.0).max(0);
    let max_x = p0.0.max(p1.0).max(p2.0).min(SIZE as i32 - 1);
    let min_y = p0.1.min(p1.1).min(p2.1).max(0);
    let max_y = p0.1.max(p1.1).max(p2.1).min(SIZE as i32 - 1);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = (x, y);
            let w0 = edge(p1, p2, p);
            let w1 = edge(p2, p0, p);
            let w2 = edge(p0, p1, p);
            let inside = (w0 >= 0 && w1 >= 0 && w2 >= 0) || (w0 <= 0 && w1 <= 0 && w2 <= 0);
            if inside {
                put(rgba, x, y, c);
            }
        }
    }
}

fn fill_quad(rgba: &mut [u8], a: P, b: P, c: P, d: P, col: [u8; 4]) {
    fill_tri(rgba, a, b, c, col);
    fill_tri(rgba, a, c, d, col);
}

fn draw_line(rgba: &mut [u8], a: P, b: P, c: [u8; 4]) {
    let mut x0 = a.0;
    let mut y0 = a.1;
    let x1 = b.0;
    let y1 = b.1;
    let dx = (x1 - x0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        put(rgba, x0, y0, c);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}
