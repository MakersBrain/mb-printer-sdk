// SPDX-License-Identifier: AGPL-3.0-or-later
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayRaster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoRaster {
    pub width: u32,
    pub height: u32,
    /// One byte per pixel: 0 for white and 1 for black.
    pub pixels: Vec<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    Auto,
    Threshold(u8),
    Bayer2,
    Bayer4,
    FloydSteinberg,
    Atkinson,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Zero,
    Clockwise90,
    Half,
    CounterClockwise90,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Left,
    Center,
    Right,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RasterError {
    #[error("pixel buffer has the wrong length")]
    Length,
    #[error("head width must be positive")]
    HeadWidth,
}

impl GrayRaster {
    pub fn new(width: u32, height: u32, white: u8) -> Self {
        Self {
            width,
            height,
            pixels: vec![white; width as usize * height as usize],
        }
    }
    pub fn validate(&self) -> Result<(), RasterError> {
        if self.pixels.len() == self.width as usize * self.height as usize {
            Ok(())
        } else {
            Err(RasterError::Length)
        }
    }
    pub fn dither(&self, mode: Dither) -> Result<MonoRaster, RasterError> {
        self.validate()?;
        let mut out = vec![0; self.pixels.len()];
        match mode {
            Dither::Auto => ordered(
                self,
                &mut out,
                &[[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]],
                16,
            ),
            Dither::Threshold(t) => {
                for (i, &v) in self.pixels.iter().enumerate() {
                    out[i] = (v < t) as u8
                }
            }
            Dither::Bayer2 => ordered(self, &mut out, &[[0, 2], [3, 1]], 4),
            Dither::Bayer4 => ordered(
                self,
                &mut out,
                &[[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]],
                16,
            ),
            Dither::FloydSteinberg => {
                let mut work: Vec<i32> = self.pixels.iter().map(|&x| x as i32 * 16).collect();
                let w = self.width as usize;
                for y in 0..self.height as usize {
                    for x in 0..w {
                        let i = y * w + x;
                        let old = work[i].clamp(0, 4080);
                        let new = if old < 2048 { 0 } else { 4080 };
                        out[i] = (new == 0) as u8;
                        let e = old - new;
                        if x + 1 < w {
                            work[i + 1] += e * 7 / 16
                        }
                        if y + 1 < self.height as usize {
                            if x > 0 {
                                work[i + w - 1] += e * 3 / 16
                            }
                            work[i + w] += e * 5 / 16;
                            if x + 1 < w {
                                work[i + w + 1] += e / 16
                            }
                        }
                    }
                }
            }
            Dither::Atkinson => {
                let mut work: Vec<i32> = self.pixels.iter().map(|&x| x as i32 * 8).collect();
                let w = self.width as usize;
                let h = self.height as usize;
                for y in 0..h {
                    for x in 0..w {
                        let i = y * w + x;
                        let old = work[i].clamp(0, 2040);
                        let new = if old < 1024 { 0 } else { 2040 };
                        out[i] = (new == 0) as u8;
                        let error = (old - new) / 8;
                        for (dx, dy) in [(1, 0), (2, 0), (-1, 1), (0, 1), (1, 1), (0, 2)] {
                            let nx = x as isize + dx;
                            let ny = y as isize + dy;
                            if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                                work[ny as usize * w + nx as usize] += error
                            }
                        }
                    }
                }
            }
        }
        Ok(MonoRaster {
            width: self.width,
            height: self.height,
            pixels: out,
        })
    }
}
fn ordered<const N: usize>(r: &GrayRaster, out: &mut [u8], m: &[[u8; N]; N], levels: u16) {
    for y in 0..r.height as usize {
        for x in 0..r.width as usize {
            let threshold = ((m[y % N][x % N] as u16 * 256 + 128) / levels) as u8;
            out[y * r.width as usize + x] = (r.pixels[y * r.width as usize + x] < threshold) as u8
        }
    }
}
impl MonoRaster {
    pub fn validate(&self) -> Result<(), RasterError> {
        if self.pixels.len() == self.width as usize * self.height as usize
            && self.pixels.iter().all(|&x| x <= 1)
        {
            Ok(())
        } else {
            Err(RasterError::Length)
        }
    }
    pub fn rotate(&self, r: Rotation) -> Self {
        match r {
            Rotation::Zero => self.clone(),
            Rotation::Half => {
                let mut p = self.pixels.clone();
                p.reverse();
                Self {
                    width: self.width,
                    height: self.height,
                    pixels: p,
                }
            }
            Rotation::Clockwise90 | Rotation::CounterClockwise90 => {
                let mut o = Self {
                    width: self.height,
                    height: self.width,
                    pixels: vec![0; self.pixels.len()],
                };
                for y in 0..self.height {
                    for x in 0..self.width {
                        let (nx, ny) = if r == Rotation::Clockwise90 {
                            (self.height - 1 - y, x)
                        } else {
                            (y, self.width - 1 - x)
                        };
                        o.pixels[(ny * o.width + nx) as usize] =
                            self.pixels[(y * self.width + x) as usize]
                    }
                }
                o
            }
        }
    }
    pub fn fit_head(&self, head_width: u32, align: Fit) -> Result<Self, RasterError> {
        self.place_on_head(head_width, align, 0, 0)
    }
    /// Match the legacy/native printer contract, whose head alignment is
    /// calculated in packed-byte columns before applying dot offsets.
    pub fn place_on_head_byte_aligned(
        &self,
        head_width: u32,
        align: Fit,
        offset_x: i32,
        offset_y: i32,
    ) -> Result<Self, RasterError> {
        if head_width == 0 || self.width > head_width {
            return Err(RasterError::HeadWidth);
        }
        let packed_width = self.width.div_ceil(8) * 8;
        let base = match align {
            Fit::Left => 0,
            Fit::Center => ((head_width / 8 - packed_width / 8) / 2) * 8,
            Fit::Right => head_width - packed_width,
        };
        self.place_on_head(head_width, Fit::Left, base as i32 + offset_x, offset_y)
    }
    pub fn place_on_head(
        &self,
        head_width: u32,
        align: Fit,
        offset_x: i32,
        offset_y: i32,
    ) -> Result<Self, RasterError> {
        if head_width == 0 {
            return Err(RasterError::HeadWidth);
        }
        let height = self.height + offset_y.max(0) as u32;
        let mut o = Self {
            width: head_width,
            height,
            pixels: vec![0; head_width as usize * height as usize],
        };
        let copy = self.width.min(head_width);
        let src = match align {
            Fit::Left => 0,
            Fit::Center => (self.width - copy) / 2,
            Fit::Right => self.width - copy,
        };
        let dst = match align {
            Fit::Left => 0,
            Fit::Center => (head_width - copy) / 2,
            Fit::Right => head_width - copy,
        };
        for y in 0..self.height {
            for x in 0..copy {
                let sx = src + x;
                let dx = dst as i64 + x as i64 + offset_x as i64;
                let dy = y as i64 + offset_y as i64;
                if dx >= 0 && dy >= 0 && dx < head_width as i64 && dy < height as i64 {
                    o.pixels[(dy as u32 * head_width + dx as u32) as usize] =
                        self.pixels[(y * self.width + sx) as usize]
                }
            }
        }
        Ok(o)
    }
    pub fn mirror_horizontal(&self) -> Self {
        let mut o = self.clone();
        for y in 0..self.height {
            for x in 0..self.width {
                o.pixels[(y * self.width + x) as usize] =
                    self.pixels[(y * self.width + self.width - 1 - x) as usize]
            }
        }
        o
    }
    pub fn pad_rows(&self, before: u32, after: u32) -> Self {
        let height = before + self.height + after;
        let mut o = Self {
            width: self.width,
            height,
            pixels: vec![0; (self.width * height) as usize],
        };
        let start = (before * self.width) as usize;
        o.pixels[start..start + self.pixels.len()].copy_from_slice(&self.pixels);
        o
    }
    pub fn pack_msb(&self) -> Result<Vec<u8>, RasterError> {
        self.validate()?;
        let stride = self.width.div_ceil(8);
        let mut out = vec![0; stride as usize * self.height as usize];
        for y in 0..self.height {
            for x in 0..self.width {
                if self.pixels[(y * self.width + x) as usize] != 0 {
                    out[(y * stride + x / 8) as usize] |= 0x80 >> (x % 8)
                }
            }
        }
        Ok(out)
    }
}
