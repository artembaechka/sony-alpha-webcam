//! RGBA → NV12 conversion with bilinear scaling and letterboxing.
//!
//! Output uses BT.709 limited range, the usual choice for HD webcams.

/// Black in limited-range YUV.
const Y_BLACK: u8 = 16;
const UV_NEUTRAL: u8 = 128;

#[inline]
fn y_of(r: i32, g: i32, b: i32) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(16, 235) as u8
}

#[inline]
fn uv_of(r: i32, g: i32, b: i32) -> (u8, u8) {
    let u = ((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    (u.clamp(16, 240) as u8, v.clamp(16, 240) as u8)
}

/// Fills an NV12 frame with black.
pub fn fill_black(dst: &mut [u8], w: usize, h: usize) {
    dst[..w * h].fill(Y_BLACK);
    dst[w * h..w * h * 3 / 2].fill(UV_NEUTRAL);
}

/// "No signal" placeholder: dark grey with a slightly lighter centre band.
pub fn fill_placeholder(dst: &mut [u8], w: usize, h: usize) {
    let (y_plane, uv) = dst.split_at_mut(w * h);
    for (row, line) in y_plane.chunks_exact_mut(w).enumerate() {
        let band = row > h * 2 / 5 && row < h * 3 / 5;
        line.fill(if band { 48 } else { 28 });
    }
    uv[..w * h / 2].fill(UV_NEUTRAL);
}

/// Precomputed bilinear sampling positions along one axis (16.16 fixed point).
struct Axis {
    idx0: Vec<usize>,
    idx1: Vec<usize>,
    frac: Vec<i32>, // 0..=256
}

impl Axis {
    fn new(src: usize, dst: usize) -> Self {
        let mut a = Axis { idx0: Vec::with_capacity(dst), idx1: Vec::with_capacity(dst), frac: Vec::with_capacity(dst) };
        let scale = src as f64 / dst as f64;
        for i in 0..dst {
            let pos = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
            let i0 = (pos.floor() as usize).min(src - 1);
            let i1 = (i0 + 1).min(src - 1);
            a.idx0.push(i0);
            a.idx1.push(i1);
            a.frac.push(((pos - i0 as f64) * 256.0).round().clamp(0.0, 256.0) as i32);
        }
        a
    }
}

/// Reusable converter (keeps the sampling tables between frames).
pub struct Converter {
    key: (usize, usize, usize, usize),
    ox: usize,
    oy: usize,
    ow: usize,
    oh: usize,
    xs: Axis,
    ys: Axis,
    rows: [Vec<i32>; 2],
}

impl Converter {
    pub fn new() -> Self {
        Self {
            key: (0, 0, 0, 0),
            ox: 0,
            oy: 0,
            ow: 0,
            oh: 0,
            xs: Axis::new(1, 1),
            ys: Axis::new(1, 1),
            rows: [Vec::new(), Vec::new()],
        }
    }

    fn prepare(&mut self, sw: usize, sh: usize, dw: usize, dh: usize) {
        if self.key == (sw, sh, dw, dh) {
            return;
        }
        self.key = (sw, sh, dw, dh);
        // Fit preserving aspect ratio, even sizes and offsets for NV12.
        let scale = (dw as f64 / sw as f64).min(dh as f64 / sh as f64);
        let ow = (((sw as f64 * scale).round() as usize) & !1).clamp(2, dw);
        let oh = (((sh as f64 * scale).round() as usize) & !1).clamp(2, dh);
        self.ox = ((dw - ow) / 2) & !1;
        self.oy = ((dh - oh) / 2) & !1;
        self.ow = ow;
        self.oh = oh;
        self.xs = Axis::new(sw, ow);
        self.ys = Axis::new(sh, oh);
        self.rows = [vec![0; ow * 3], vec![0; ow * 3]];
    }

    /// Scales `src` (RGBA, `sw`×`sh`) into the NV12 frame `dst` (`dw`×`dh`).
    pub fn rgba_to_nv12(&mut self, src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
        assert!(src.len() >= sw * sh * 4, "source too small");
        assert!(dst.len() >= dw * dh * 3 / 2, "destination too small");
        assert!(dw.is_multiple_of(2) && dh.is_multiple_of(2), "NV12 needs even dimensions");
        self.prepare(sw, sh, dw, dh);
        if self.ow != dw || self.oh != dh {
            fill_black(dst, dw, dh);
        }
        let (ox, oy, ow, oh) = (self.ox, self.oy, self.ow, self.oh);
        let (y_plane, uv_plane) = dst.split_at_mut(dw * dh);

        for pair in 0..oh / 2 {
            for k in 0..2 {
                let oyr = pair * 2 + k;
                let (r0, r1, fy) = (self.ys.idx0[oyr], self.ys.idx1[oyr], self.ys.frac[oyr]);
                let row0 = &src[r0 * sw * 4..(r0 + 1) * sw * 4];
                let row1 = &src[r1 * sw * 4..(r1 + 1) * sw * 4];
                let out = &mut self.rows[k];
                for x in 0..ow {
                    let (c0, c1, fx) = (self.xs.idx0[x] * 4, self.xs.idx1[x] * 4, self.xs.frac[x]);
                    for c in 0..3 {
                        let top = row0[c0 + c] as i32 * (256 - fx) + row0[c1 + c] as i32 * fx;
                        let bot = row1[c0 + c] as i32 * (256 - fx) + row1[c1 + c] as i32 * fx;
                        out[x * 3 + c] = (top * (256 - fy) + bot * fy + (1 << 15)) >> 16;
                    }
                }
                let yline = &mut y_plane[(oy + oyr) * dw + ox..(oy + oyr) * dw + ox + ow];
                for (x, yv) in yline.iter_mut().enumerate() {
                    *yv = y_of(out[x * 3], out[x * 3 + 1], out[x * 3 + 2]);
                }
            }
            let uv_row = (oy / 2 + pair) * dw;
            let (a, b) = (&self.rows[0], &self.rows[1]);
            for x in (0..ow).step_by(2) {
                let avg = |c: usize| (a[x * 3 + c] + a[x * 3 + 3 + c] + b[x * 3 + c] + b[x * 3 + 3 + c] + 2) >> 2;
                let (u, v) = uv_of(avg(0), avg(1), avg(2));
                let i = uv_row + ox + x;
                uv_plane[i] = u;
                uv_plane[i + 1] = v;
            }
        }
    }
}

impl Default for Converter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_and_black() {
        let mut conv = Converter::new();
        let src = vec![255u8; 4 * 4 * 4];
        let mut dst = vec![0u8; 8 * 4 * 3 / 2];
        conv.rgba_to_nv12(&src, 4, 4, &mut dst, 8, 4);
        // 4x4 source letterboxed into 8x4: centre is white, sides black.
        assert_eq!(dst[0], 16);
        assert_eq!(dst[3], 235);
        assert_eq!(dst[7], 16);
        assert!(dst[32..].iter().all(|&v| (127..=129).contains(&v)));
    }

    #[test]
    fn upscale_16_9() {
        let mut conv = Converter::new();
        let (sw, sh) = (1024, 576);
        let mut src = vec![0u8; sw * sh * 4];
        for px in src.chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        let mut dst = vec![0u8; super::super::FRAME_BYTES];
        conv.rgba_to_nv12(&src, sw, sh, &mut dst, 1280, 720);
        // Pure red, BT.709 limited range: Y≈63, U≈102, V≈240.
        let y = dst[640 * 1280 / 2];
        assert!((60..=66).contains(&y), "y = {y}");
        let uv = &dst[1280 * 720..];
        assert!((99..=105).contains(&uv[0]), "u = {}", uv[0]);
        assert!(uv[1] >= 236, "v = {}", uv[1]);
    }
}
