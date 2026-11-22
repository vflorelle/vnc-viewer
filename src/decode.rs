//! Image de l'écran distant et décodage des rectangles reçus : Raw, CopyRect, RRE, Hextile,
//! Zlib, ZRLE, Tight (dont JPEG), et pseudo-encodages (curseur, taille d'écran, nom).

use std::io::{self, Read};

use eframe::egui::Color32;
use flate2::{Decompress, FlushDecompress, Status};

use crate::rfb::{PixelFormat, ReadExt, protocol_error};

pub const RAW: i32 = 0;
pub const COPY_RECT: i32 = 1;
pub const RRE: i32 = 2;
pub const HEXTILE: i32 = 5;
pub const ZLIB: i32 = 6;
pub const TIGHT: i32 = 7;
pub const ZRLE: i32 = 16;
/// Qualité JPEG de Tight : de -32 (0, la plus basse) à -23 (9).
pub const JPEG_QUALITY_0: i32 = -32;
/// Compression de Tight : de -256 (0, la plus rapide) à -247 (9).
pub const COMPRESS_LEVEL_0: i32 = -256;
pub const CURSOR: i32 = -239;
/// Curseur à transparence progressive (bords lissés), en RGBA prémultiplié.
pub const CURSOR_WITH_ALPHA: i32 = -314;
pub const DESKTOP_SIZE: i32 = -223;
pub const LAST_RECT: i32 = -224;
pub const DESKTOP_NAME: i32 = -307;
pub const EXTENDED_DESKTOP_SIZE: i32 = -308;
pub const EXTENDED_CLIPBOARD: i32 = 0xc0a1_e5ce_u32 as i32;
/// Boutons de souris au-delà de 8 (« suivant »).
pub const EXTENDED_MOUSE_BUTTONS: i32 = -316;

/// Format des pixels d'un curseur à transparence : 4 octets R, V, B, A (32 bits utilisés, donc
/// jamais réduits à 3 octets par ZRLE ou Tight).
const RGBA: PixelFormat = PixelFormat {
    bits_per_pixel: 32,
    depth: 32,
    big_endian: false,
    true_colour: true,
    red_max: 255,
    green_max: 255,
    blue_max: 255,
    red_shift: 0,
    green_shift: 8,
    blue_shift: 16,
};

/// Taille maximale acceptée pour l'écran distant (pixels de côté).
const MAX_SIDE: usize = 16384;

/// Image de l'écran distant.
#[derive(Clone, Default)]
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Color32>,
    /// Zone modifiée depuis la dernière copie : x0, y0, x1, y1 (exclus).
    pub dirty: Option<[usize; 4]>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Framebuffer {
            width,
            height,
            pixels: vec![Color32::BLACK; width * height],
            dirty: Some([0, 0, width, height]),
        }
    }

    pub fn mark(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        if x >= x1 || y >= y1 {
            return;
        }
        self.dirty = Some(match self.dirty {
            Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x1), d.max(y1)],
            None => [x, y, x1, y1],
        });
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, color: Color32) {
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        for row in y.min(y1)..y1 {
            self.pixels[row * self.width + x.min(x1)..row * self.width + x1].fill(color);
        }
        self.mark(x, y, w, h);
    }

    /// Pixels `w × h` ligne par ligne, rognés aux bords de l'écran.
    pub fn put(&mut self, x: usize, y: usize, w: usize, h: usize, pixels: &[Color32]) {
        let visible = w.min(self.width.saturating_sub(x));
        for row in 0..h.min(self.height.saturating_sub(y)) {
            let dst = (y + row) * self.width + x;
            self.pixels[dst..dst + visible].copy_from_slice(&pixels[row * w..row * w + visible]);
        }
        self.mark(x, y, w, h);
    }

    fn copy(&mut self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
        let w = w.min(self.width.saturating_sub(sx)).min(self.width.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(sy)).min(self.height.saturating_sub(y));
        let rows: Vec<usize> = if sy < y {
            (0..h).rev().collect()
        } else {
            (0..h).collect()
        };
        for row in rows {
            let src = (sy + row) * self.width + sx;
            self.pixels.copy_within(src..src + w, (y + row) * self.width + x);
        }
        self.mark(x, y, w, h);
    }

    /// Copie la zone modifiée de `self` dans `target` (de même taille), puis l'oublie.
    pub fn flush_into(&mut self, target: &mut Framebuffer) {
        if target.width != self.width || target.height != self.height {
            *target = self.clone();
            target.dirty = Some([0, 0, self.width, self.height]);
            self.dirty = None;
            return;
        }
        let Some([x0, y0, x1, y1]) = self.dirty.take() else {
            return;
        };
        for row in y0..y1 {
            let range = row * self.width + x0..row * self.width + x1;
            target.pixels[range.clone()].copy_from_slice(&self.pixels[range]);
        }
        target.mark(x0, y0, x1 - x0, y1 - y0);
    }
}

/// Forme du curseur distant (RGBA non prémultiplié).
#[derive(Clone, PartialEq)]
pub struct Cursor {
    pub width: usize,
    pub height: usize,
    pub hotspot: [usize; 2],
    pub rgba: Vec<u8>,
}

impl Cursor {
    /// Point actif en (`x`, `y`), ramené dans l'image.
    fn new(width: usize, height: usize, x: usize, y: usize, rgba: Vec<u8>) -> Self {
        Cursor {
            width,
            height,
            hotspot: [x.min(width.saturating_sub(1)), y.min(height.saturating_sub(1))],
            rgba,
        }
    }
}

/// Ce qu'un rectangle change en dehors des pixels.
pub enum Change {
    Pixels,
    Resized,
    Cursor(Cursor),
    Name(String),
    /// Le serveur gère les boutons de souris étendus.
    ExtendedMouse,
    /// Fin de la mise à jour (le nombre de rectangles annoncé n'est pas atteint).
    LastRect,
}

pub struct Decoder {
    format: PixelFormat,
    /// Couleur de chaque valeur de pixel, pour 8 et 16 bits.
    table: Vec<Color32>,
    zlib: Decompress,
    zrle: Decompress,
    tight: [Decompress; 4],
    /// Pixels R, V, B, A (curseur à transparence) au lieu du format de l'écran.
    rgba: bool,
}

impl Decoder {
    pub fn new(format: PixelFormat) -> Self {
        let mut decoder = Decoder {
            format,
            table: Vec::new(),
            zlib: Decompress::new(true),
            zrle: Decompress::new(true),
            tight: std::array::from_fn(|_| Decompress::new(true)),
            rgba: false,
        };
        decoder.set_format(format);
        decoder
    }

    pub fn set_format(&mut self, format: PixelFormat) {
        self.format = format;
        self.table = if format.bits_per_pixel <= 16 {
            (0..1u32 << format.bits_per_pixel)
                .map(|v| convert(&format, v))
                .collect()
        } else {
            Vec::new()
        };
    }

    fn color(&self, value: u32) -> Color32 {
        match self.table.get(value as usize) {
            Some(c) => *c,
            None => convert(&self.format, value),
        }
    }

    /// Valeur d'un pixel complet.
    fn value(&self, b: &[u8]) -> u32 {
        match (b.len(), self.format.big_endian) {
            (1, _) => u32::from(b[0]),
            (2, false) => u32::from(u16::from_le_bytes([b[0], b[1]])),
            (2, true) => u32::from(u16::from_be_bytes([b[0], b[1]])),
            (_, false) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (_, true) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }

    fn pixels(&self, data: &[u8], size: usize, to: impl Fn(&Self, &[u8]) -> Color32) -> Vec<Color32> {
        data.chunks_exact(size).map(|p| to(self, p)).collect()
    }

    fn full(&self, b: &[u8]) -> Color32 {
        if self.rgba {
            return Color32::from_rgba_premultiplied(b[0], b[1], b[2], b[3]);
        }
        self.color(self.value(b))
    }

    /// Pixel compact de ZRLE : 3 octets si le format 32 bits n'en utilise que 3.
    fn cpixel_size(&self) -> usize {
        let f = &self.format;
        if f.true_colour && f.bits_per_pixel == 32 && f.depth <= 24 {
            3
        } else {
            f.bytes_per_pixel()
        }
    }

    fn cpixel(&self, b: &[u8]) -> Color32 {
        if b.len() != 3 {
            return self.full(b);
        }
        let f = &self.format;
        let used = (u32::from(f.red_max) << f.red_shift)
            | (u32::from(f.green_max) << f.green_shift)
            | (u32::from(f.blue_max) << f.blue_shift);
        // Les 3 octets sont les poids faibles, ou les poids forts, du pixel.
        let low = used < 1 << 24;
        let full = match (f.big_endian, low) {
            (false, true) => [b[0], b[1], b[2], 0],
            (false, false) => [0, b[0], b[1], b[2]],
            (true, true) => [0, b[0], b[1], b[2]],
            (true, false) => [b[0], b[1], b[2], 0],
        };
        self.full(&full)
    }

    /// Pixel de Tight : rouge, vert, bleu sur 3 octets pour un format 24 bits.
    fn is_rgb24(&self) -> bool {
        let f = &self.format;
        f.true_colour
            && f.bits_per_pixel == 32
            && f.depth == 24
            && [f.red_max, f.green_max, f.blue_max] == [255, 255, 255]
    }

    fn tpixel_size(&self) -> usize {
        if self.is_rgb24() {
            3
        } else {
            self.format.bytes_per_pixel()
        }
    }

    fn tpixel(&self, b: &[u8]) -> Color32 {
        if b.len() == 3 {
            Color32::from_rgb(b[0], b[1], b[2])
        } else {
            self.full(b)
        }
    }

    /// Lit un rectangle et l'applique à `fb`.
    pub fn rect(
        &mut self,
        input: &mut dyn Read,
        fb: &mut Framebuffer,
        (x, y, w, h): (usize, usize, usize, usize),
        encoding: i32,
    ) -> io::Result<Change> {
        let bpp = self.format.bytes_per_pixel();
        match encoding {
            RAW => {
                let data = input.read_vec(w * h * bpp)?;
                fb.put(x, y, w, h, &self.pixels(&data, bpp, Self::full));
            }
            COPY_RECT => {
                let sx = usize::from(input.u16()?);
                let sy = usize::from(input.u16()?);
                fb.copy(sx, sy, x, y, w, h);
            }
            RRE => {
                let count = input.u32()?;
                let background = self.full(&input.read_vec(bpp)?);
                fb.fill(x, y, w, h, background);
                for _ in 0..count {
                    let color = self.full(&input.read_vec(bpp)?);
                    let (sx, sy) = (usize::from(input.u16()?), usize::from(input.u16()?));
                    let (sw, sh) = (usize::from(input.u16()?), usize::from(input.u16()?));
                    fb.fill(
                        x + sx,
                        y + sy,
                        sw.min(w.saturating_sub(sx)),
                        sh.min(h.saturating_sub(sy)),
                        color,
                    );
                }
            }
            HEXTILE => self.hextile(input, fb, (x, y, w, h))?,
            ZLIB => {
                let len = input.u32()? as usize;
                let data = inflate(&mut self.zlib, &input.read_vec(len)?, Some(w * h * bpp))?;
                fb.put(x, y, w, h, &self.pixels(&data, bpp, Self::full));
            }
            ZRLE => {
                let len = input.u32()? as usize;
                let data = inflate(&mut self.zrle, &input.read_vec(len)?, None)?;
                self.zrle_tiles(&mut io::Cursor::new(data), fb, (x, y, w, h))?;
            }
            TIGHT => self.tight(input, fb, (x, y, w, h))?,
            CURSOR => {
                let pixels = input.read_vec(w * h * bpp)?;
                let mask = input.read_vec(w.div_ceil(8) * h)?;
                let mut rgba = Vec::with_capacity(w * h * 4);
                for (i, p) in pixels.chunks_exact(bpp).enumerate() {
                    let (row, col) = (i / w.max(1), i % w.max(1));
                    let visible = mask[row * w.div_ceil(8) + col / 8] & (0x80 >> (col % 8)) != 0;
                    let c = self.full(p);
                    rgba.extend_from_slice(&[c.r(), c.g(), c.b(), if visible { 255 } else { 0 }]);
                }
                return Ok(Change::Cursor(Cursor::new(w, h, x, y, rgba)));
            }
            CURSOR_WITH_ALPHA => {
                // Un encodage d'image quelconque (Raw, ZRLE…, avec les flux zlib de la
                // connexion), dont les pixels sont 4 octets R, V, B, A, couleurs prémultipliées.
                let encoding = input.i32()?;
                // Un curseur vide (0 × 0, caché pendant la frappe) a aussi son encodage et ses
                // données, vides.
                if encoding < 0 {
                    return Err(protocol_error(tr!(
                        "curseur dans un encodage inattendu ({encoding})",
                        "cursor in an unexpected encoding ({encoding})"
                    )));
                }
                let format = std::mem::replace(&mut self.format, RGBA);
                self.rgba = true;
                let mut image = Framebuffer::new(w, h);
                let result = self.rect(input, &mut image, (0, 0, w, h), encoding);
                self.rgba = false;
                self.format = format;
                result?;
                let rgba = image.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                return Ok(Change::Cursor(Cursor::new(w, h, x, y, rgba)));
            }
            DESKTOP_SIZE => {
                resize(fb, w, h)?;
                return Ok(Change::Resized);
            }
            EXTENDED_DESKTOP_SIZE => {
                let screens = input.u8()?;
                input.skip(3 + 16 * usize::from(screens))?;
                // y : 0 si le changement a réussi (x : qui l'a demandé).
                if y == 0 {
                    resize(fb, w, h)?;
                }
                return Ok(Change::Resized);
            }
            DESKTOP_NAME => {
                let len = input.u32()?;
                let name = input.read_vec(len.min(1 << 16) as usize)?;
                return Ok(Change::Name(String::from_utf8_lossy(&name).into_owned()));
            }
            LAST_RECT => return Ok(Change::LastRect),
            EXTENDED_MOUSE_BUTTONS => return Ok(Change::ExtendedMouse),
            other => {
                return Err(protocol_error(tr!(
                    "encodage inattendu ({other})",
                    "unexpected encoding ({other})"
                )));
            }
        }
        Ok(Change::Pixels)
    }

    fn hextile(
        &mut self,
        input: &mut dyn Read,
        fb: &mut Framebuffer,
        (x, y, w, h): (usize, usize, usize, usize),
    ) -> io::Result<()> {
        const RAW: u8 = 1;
        const BACKGROUND: u8 = 2;
        const FOREGROUND: u8 = 4;
        const SUBRECTS: u8 = 8;
        const COLOURED: u8 = 16;
        let bpp = self.format.bytes_per_pixel();
        let (mut background, mut foreground) = (Color32::BLACK, Color32::BLACK);
        for ty in (y..y + h).step_by(16) {
            for tx in (x..x + w).step_by(16) {
                let (tw, th) = ((x + w - tx).min(16), (y + h - ty).min(16));
                let flags = input.u8()?;
                if flags & RAW != 0 {
                    let data = input.read_vec(tw * th * bpp)?;
                    fb.put(tx, ty, tw, th, &self.pixels(&data, bpp, Self::full));
                    continue;
                }
                if flags & BACKGROUND != 0 {
                    background = self.full(&input.read_vec(bpp)?);
                }
                fb.fill(tx, ty, tw, th, background);
                if flags & FOREGROUND != 0 {
                    foreground = self.full(&input.read_vec(bpp)?);
                }
                if flags & SUBRECTS != 0 {
                    for _ in 0..input.u8()? {
                        let color = if flags & COLOURED != 0 {
                            self.full(&input.read_vec(bpp)?)
                        } else {
                            foreground
                        };
                        let (xy, wh) = (usize::from(input.u8()?), usize::from(input.u8()?));
                        let (sx, sy) = (xy >> 4, xy & 15);
                        let (sw, sh) = ((wh >> 4) + 1, (wh & 15) + 1);
                        fb.fill(
                            tx + sx,
                            ty + sy,
                            sw.min(tw.saturating_sub(sx)),
                            sh.min(th.saturating_sub(sy)),
                            color,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn zrle_tiles(
        &self,
        data: &mut io::Cursor<Vec<u8>>,
        fb: &mut Framebuffer,
        (x, y, w, h): (usize, usize, usize, usize),
    ) -> io::Result<()> {
        let size = self.cpixel_size();
        let mut tile = Vec::with_capacity(64 * 64);
        for ty in (y..y + h).step_by(64) {
            for tx in (x..x + w).step_by(64) {
                let (tw, th) = ((x + w - tx).min(64), (y + h - ty).min(64));
                let count = tw * th;
                let sub = data.u8()?;
                let mut palette = Vec::new();
                let palette_size = match sub {
                    2..=16 => usize::from(sub),
                    130..=255 => usize::from(sub - 128),
                    _ => 0,
                };
                for _ in 0..palette_size {
                    palette.push(self.cpixel(&data.read_vec(size)?));
                }
                tile.clear();
                match sub {
                    0 => tile.extend(self.pixels(&data.read_vec(count * size)?, size, Self::cpixel)),
                    1 => tile.resize(count, self.cpixel(&data.read_vec(size)?)),
                    2..=16 => {
                        let bits = match palette_size {
                            2 => 1,
                            3..=4 => 2,
                            _ => 4,
                        };
                        let row_bytes = (tw * bits).div_ceil(8);
                        let packed = data.read_vec(row_bytes * th)?;
                        for row in packed.chunks_exact(row_bytes) {
                            for col in 0..tw {
                                let bit = col * bits;
                                let index = (row[bit / 8] >> (8 - bits - bit % 8)) & ((1 << bits) - 1);
                                tile.push(palette.get(usize::from(index)).copied().unwrap_or_default());
                            }
                        }
                    }
                    128 | 130..=255 => {
                        while tile.len() < count {
                            let (color, run) = if sub == 128 {
                                (self.cpixel(&data.read_vec(size)?), run_length(data)?)
                            } else {
                                let index = data.u8()?;
                                let color = palette.get(usize::from(index & 127)).copied().unwrap_or_default();
                                (color, if index & 128 != 0 { run_length(data)? } else { 1 })
                            };
                            tile.extend(std::iter::repeat_n(color, run.min(count - tile.len())));
                        }
                    }
                    _ => {
                        return Err(protocol_error(tr!(
                            "tuile ZRLE invalide ({sub})",
                            "invalid ZRLE tile ({sub})"
                        )));
                    }
                }
                fb.put(tx, ty, tw, th, &tile);
            }
        }
        Ok(())
    }

    fn tight(
        &mut self,
        input: &mut dyn Read,
        fb: &mut Framebuffer,
        (x, y, w, h): (usize, usize, usize, usize),
    ) -> io::Result<()> {
        const FILL: u8 = 8;
        const JPEG: u8 = 9;
        const COPY: u8 = 0;
        const PALETTE: u8 = 1;
        const GRADIENT: u8 = 2;
        let control = input.u8()?;
        for (i, stream) in self.tight.iter_mut().enumerate() {
            if control & (1 << i) != 0 {
                *stream = Decompress::new(true);
            }
        }
        let size = self.tpixel_size();
        match control >> 4 {
            FILL => {
                let color = self.tpixel(&input.read_vec(size)?);
                fb.fill(x, y, w, h, color);
            }
            JPEG => {
                let len = compact_length(input)?;
                let pixels = jpeg(&input.read_vec(len)?, w, h)?;
                fb.put(x, y, w, h, &pixels);
            }
            method if method < 8 => {
                let filter = if method & 4 != 0 { input.u8()? } else { COPY };
                let stream = usize::from(method & 3);
                match filter {
                    COPY => {
                        let data = self.tight_data(input, stream, w * h * size)?;
                        fb.put(x, y, w, h, &self.pixels(&data, size, Self::tpixel));
                    }
                    PALETTE => {
                        let colors = usize::from(input.u8()?) + 1;
                        let palette = self.pixels(&input.read_vec(colors * size)?, size, Self::tpixel);
                        let pixels = if colors == 2 {
                            let row_bytes = w.div_ceil(8);
                            let data = self.tight_data(input, stream, row_bytes * h)?;
                            data.chunks_exact(row_bytes)
                                .flat_map(|row| (0..w).map(move |col| (row[col / 8] >> (7 - col % 8)) & 1))
                                .map(|i| palette[usize::from(i)])
                                .collect()
                        } else {
                            let data = self.tight_data(input, stream, w * h)?;
                            data.iter()
                                .map(|&i| palette.get(usize::from(i)).copied().unwrap_or_default())
                                .collect::<Vec<_>>()
                        };
                        fb.put(x, y, w, h, &pixels);
                    }
                    GRADIENT => {
                        let data = self.tight_data(input, stream, w * h * size)?;
                        fb.put(x, y, w, h, &self.gradient(&data, w, size));
                    }
                    other => {
                        return Err(protocol_error(tr!(
                            "filtre Tight inconnu ({other})",
                            "unknown Tight filter ({other})"
                        )));
                    }
                }
            }
            other => {
                return Err(protocol_error(tr!(
                    "compression Tight inconnue ({other})",
                    "unknown Tight compression ({other})"
                )));
            }
        }
        Ok(())
    }

    /// Données de Tight : en clair sous 12 octets, sinon compressées dans le flux `stream`.
    fn tight_data(&mut self, input: &mut dyn Read, stream: usize, len: usize) -> io::Result<Vec<u8>> {
        if len < 12 {
            return input.read_vec(len);
        }
        let compressed = compact_length(input)?;
        inflate(&mut self.tight[stream], &input.read_vec(compressed)?, Some(len))
    }

    /// Filtre « gradient » de Tight : chaque composante est l'écart à la prédiction
    /// gauche + haut − haut-gauche.
    fn gradient(&self, data: &[u8], w: usize, size: usize) -> Vec<Color32> {
        let f = self.format;
        let rgb24 = size == 3;
        let max: [u32; 3] = if rgb24 {
            [255; 3]
        } else {
            [f.red_max, f.green_max, f.blue_max].map(u32::from)
        };
        let components = |p: &[u8]| -> [u32; 3] {
            if rgb24 {
                [p[0], p[1], p[2]].map(u32::from)
            } else {
                let v = self.value(p);
                [
                    (v >> f.red_shift) & max[0],
                    (v >> f.green_shift) & max[1],
                    (v >> f.blue_shift) & max[2],
                ]
            }
        };
        let mut above = vec![[0u32; 3]; w];
        let mut current = vec![[0u32; 3]; w];
        let mut out = Vec::with_capacity(data.len() / size);
        for row in data.chunks_exact(w * size) {
            for (col, p) in row.chunks_exact(size).enumerate() {
                let delta = components(p);
                for c in 0..3 {
                    let left = if col > 0 { current[col - 1][c] } else { 0 };
                    let corner = if col > 0 { above[col - 1][c] } else { 0 };
                    let predicted = (i64::from(left) + i64::from(above[col][c]) - i64::from(corner))
                        .clamp(0, i64::from(max[c])) as u32;
                    current[col][c] = (predicted + delta[c]) & max[c];
                }
                let [r, g, b] = current[col];
                out.push(Color32::from_rgb(scale(r, max[0]), scale(g, max[1]), scale(b, max[2])));
            }
            std::mem::swap(&mut above, &mut current);
        }
        out
    }
}

fn scale(value: u32, max: u32) -> u8 {
    (value * 255).checked_div(max).unwrap_or(0) as u8
}

fn convert(f: &PixelFormat, value: u32) -> Color32 {
    let c = |shift: u8, max: u16| scale((value >> shift) & u32::from(max), u32::from(max));
    Color32::from_rgb(
        c(f.red_shift, f.red_max),
        c(f.green_shift, f.green_max),
        c(f.blue_shift, f.blue_max),
    )
}

fn resize(fb: &mut Framebuffer, w: usize, h: usize) -> io::Result<()> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(protocol_error(tr!(
            "taille d'écran invalide ({w} × {h})",
            "invalid screen size ({w} × {h})"
        )));
    }
    *fb = Framebuffer::new(w, h);
    Ok(())
}

/// Longueur d'une plage RLE : somme d'octets, 255 signifiant « encore », plus un.
fn run_length(data: &mut dyn Read) -> io::Result<usize> {
    let mut run = 1;
    loop {
        let b = data.u8()?;
        run += usize::from(b);
        if b != 255 {
            return Ok(run);
        }
    }
}

/// Longueur compacte de Tight : 1 à 3 octets de 7 bits.
fn compact_length(input: &mut dyn Read) -> io::Result<usize> {
    let mut len = 0;
    for i in 0..3 {
        let b = input.u8()?;
        if i == 2 {
            return Ok(len | usize::from(b) << 14);
        }
        len |= usize::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok(len)
}

/// Décompresse un bloc d'un flux zlib continu ; `expected` : taille exacte attendue.
fn inflate(stream: &mut Decompress, input: &[u8], expected: Option<usize>) -> io::Result<Vec<u8>> {
    let error = || protocol_error(tr!("données compressées invalides", "invalid compressed data"));
    let mut out = Vec::with_capacity(expected.unwrap_or(input.len() * 4).max(64));
    let start = stream.total_in();
    loop {
        let consumed = (stream.total_in() - start) as usize;
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(4096));
        }
        let before = (stream.total_in(), stream.total_out());
        let status = stream
            .decompress_vec(&input[consumed..], &mut out, FlushDecompress::Sync)
            .map_err(|_| error())?;
        // Fini : fin du flux, toute l'entrée lue sans remplir la sortie, ou plus rien ne bouge.
        let all_read = (stream.total_in() - start) as usize == input.len();
        let stuck = before == (stream.total_in(), stream.total_out());
        if status == Status::StreamEnd || (all_read && out.len() < out.capacity()) || stuck {
            break;
        }
    }
    match expected {
        Some(e) if out.len() < e => Err(error()),
        Some(e) => {
            out.truncate(e);
            Ok(out)
        }
        None => Ok(out),
    }
}

fn jpeg(data: &[u8], w: usize, h: usize) -> io::Result<Vec<Color32>> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let error = |e: String| protocol_error(tr!("image JPEG invalide ({e})", "invalid JPEG image ({e})"));
    let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(data));
    let rgb = decoder.decode().map_err(|e| error(e.to_string()))?;
    if decoder.dimensions() != Some((w, h)) || rgb.len() != w * h * 3 {
        return Err(error(format!("{w} × {h}")));
    }
    Ok(rgb
        .as_chunks::<3>()
        .0
        .iter()
        .map(|[r, g, b]| Color32::from_rgb(*r, *g, *b))
        .collect())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    /// Compression d'un bloc dans un flux zlib continu (comme le serveur).
    pub fn deflate(stream: &mut Compress, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 1024);
        stream.compress_vec(data, &mut out, FlushCompress::Sync).unwrap();
        out
    }

    fn rect(decoder: &mut Decoder, fb: &mut Framebuffer, r: (usize, usize, usize, usize), encoding: i32, data: &[u8]) {
        let mut input = io::Cursor::new(data.to_vec());
        decoder.rect(&mut input, fb, r, encoding).unwrap();
        assert_eq!(input.position() as usize, data.len(), "données non consommées");
    }

    const RED: Color32 = Color32::from_rgb(255, 0, 0);
    const BLUE: Color32 = Color32::from_rgb(0, 0, 255);

    #[test]
    fn encodings() {
        let mut d = Decoder::new(PixelFormat::RGB888);
        let mut fb = Framebuffer::new(80, 70);
        let red = [0, 0, 255, 0];
        let blue = [255, 0, 0, 0];
        // Raw 2×1.
        rect(&mut d, &mut fb, (0, 0, 2, 1), RAW, &[red, blue].concat());
        assert_eq!(&fb.pixels[..2], &[RED, BLUE]);
        // CopyRect qui se recouvre.
        rect(&mut d, &mut fb, (1, 0, 2, 1), COPY_RECT, &[0, 0, 0, 0]);
        assert_eq!(&fb.pixels[..3], &[RED, RED, BLUE]);
        // RRE : fond bleu, sous-rectangle rouge 1×1 en (1, 1).
        let mut rre = 1u32.to_be_bytes().to_vec();
        rre.extend_from_slice(&blue);
        rre.extend_from_slice(&red);
        for v in [1u16, 1, 1, 1] {
            rre.extend_from_slice(&v.to_be_bytes());
        }
        rect(&mut d, &mut fb, (10, 10, 3, 3), RRE, &rre);
        assert_eq!(fb.pixels[11 * 80 + 11], RED);
        assert_eq!(fb.pixels[10 * 80 + 10], BLUE);
        // Hextile : tuile 20×2 → deux tuiles ; fond rouge, sous-rectangle de premier plan.
        let mut hex = vec![2 | 4 | 8];
        hex.extend_from_slice(&red);
        hex.extend_from_slice(&blue);
        hex.extend_from_slice(&[1, 0x10, 0x10]); // 1 sous-rectangle en (1, 0), 2×1
        hex.push(0); // seconde tuile : fond conservé
        rect(&mut d, &mut fb, (0, 20, 20, 2), HEXTILE, &hex);
        assert_eq!(&fb.pixels[20 * 80..20 * 80 + 4], &[RED, BLUE, BLUE, RED]);
        assert_eq!(fb.pixels[21 * 80 + 19], RED);

        // ZRLE : une tuile 2×2 en RLE de palette, puis une tuile unie dans un second bloc.
        let mut z = Compress::new(Compression::default(), true);
        let tile = [130, 0, 0, 255, 255, 0, 0, 0x80, 2, 1];
        let block = deflate(&mut z, &tile);
        let mut zrle = (block.len() as u32).to_be_bytes().to_vec();
        zrle.extend_from_slice(&block);
        rect(&mut d, &mut fb, (40, 40, 2, 2), ZRLE, &zrle);
        assert_eq!(&fb.pixels[40 * 80 + 40..40 * 80 + 42], &[RED, RED]);
        assert_eq!(&fb.pixels[41 * 80 + 40..41 * 80 + 42], &[RED, BLUE]);
        let block = deflate(&mut z, &[1, 255, 0, 0]);
        let mut zrle = (block.len() as u32).to_be_bytes().to_vec();
        zrle.extend_from_slice(&block);
        rect(&mut d, &mut fb, (40, 40, 1, 1), ZRLE, &zrle);
        assert_eq!(fb.pixels[40 * 80 + 40], BLUE);

        // Tight : remplissage, puis palette 2 couleurs compressée (≥ 12 octets).
        rect(&mut d, &mut fb, (0, 50, 4, 4), TIGHT, &[0x80, 0, 0, 255]);
        assert_eq!(fb.pixels[53 * 80 + 3], BLUE);
        let mut t = Compress::new(Compression::default(), true);
        let bits = vec![0b1010_0000; 16];
        let block = deflate(&mut t, &bits);
        let mut tight = vec![0x40 | 0x10, 1, 1, 255, 0, 0, 0, 0, 255];
        tight.push(block.len() as u8);
        tight.extend_from_slice(&block);
        rect(&mut d, &mut fb, (0, 50, 4, 16), TIGHT, &tight);
        assert_eq!(&fb.pixels[50 * 80..50 * 80 + 4], &[BLUE, RED, BLUE, RED]);
        // Gradient non compressé (< 12 octets) : 2 pixels, le second prédit par le premier.
        let tight = [0x40, 2, 10, 20, 30, 1, 1, 1];
        rect(&mut d, &mut fb, (0, 60, 2, 1), TIGHT, &tight);
        assert_eq!(
            &fb.pixels[60 * 80..60 * 80 + 2],
            &[Color32::from_rgb(10, 20, 30), Color32::from_rgb(11, 21, 31)]
        );

        assert!(matches!(
            d.rect(&mut io::Cursor::new(vec![]), &mut fb, (0, 0, 100, 50), DESKTOP_SIZE)
                .ok(),
            Some(Change::Resized)
        ));
        assert_eq!((fb.width, fb.height), (100, 50));
    }

    #[test]
    fn low_colour_formats() {
        let mut d = Decoder::new(PixelFormat::BGR233);
        let mut fb = Framebuffer::new(2, 1);
        rect(&mut d, &mut fb, (0, 0, 2, 1), RAW, &[0x07, 0xc0]);
        assert_eq!(&fb.pixels, &[RED, BLUE]);
        d.set_format(PixelFormat::RGB565);
        rect(&mut d, &mut fb, (0, 0, 2, 1), RAW, &[0x00, 0xf8, 0x1f, 0x00]);
        assert_eq!(&fb.pixels, &[RED, BLUE]);
    }

    #[test]
    fn cursor_and_flush() {
        let mut d = Decoder::new(PixelFormat::RGB888);
        let mut fb = Framebuffer::new(4, 4);
        let mut data = [[0, 0, 255, 0]; 4].concat();
        data.extend_from_slice(&[0x80, 0x40]);
        let Ok(Change::Cursor(c)) = d.rect(&mut io::Cursor::new(data), &mut fb, (1, 0, 2, 2), CURSOR) else {
            panic!("curseur")
        };
        assert_eq!(c.hotspot, [1, 0]);
        assert_eq!(c.rgba[3], 255);
        assert_eq!(c.rgba[7], 0);
        assert_eq!(c.rgba[15], 255);
        // Curseur à transparence : couleurs prémultipliées ramenées à leur valeur.
        let mut data = RAW.to_be_bytes().to_vec();
        data.extend_from_slice(&[128, 0, 0, 128]);
        let Ok(Change::Cursor(c)) = d.rect(&mut io::Cursor::new(data), &mut fb, (0, 0, 1, 1), CURSOR_WITH_ALPHA) else {
            panic!("curseur à transparence")
        };
        assert_eq!(c.rgba, [255, 0, 0, 128]);
        // Même curseur en ZRLE (tuile unie), pixel compact sur 4 octets.
        let mut z = Compress::new(Compression::default(), true);
        let block = deflate(&mut z, &[1, 128, 0, 0, 128]);
        let mut data = ZRLE.to_be_bytes().to_vec();
        data.extend_from_slice(&(block.len() as u32).to_be_bytes());
        data.extend_from_slice(&block);
        let Ok(Change::Cursor(c)) = d.rect(&mut io::Cursor::new(data), &mut fb, (0, 0, 2, 1), CURSOR_WITH_ALPHA) else {
            panic!("curseur à transparence en ZRLE")
        };
        assert_eq!(c.rgba, [255, 0, 0, 128, 255, 0, 0, 128]);
        // Curseur caché : vide, avec son encodage et des données vides.
        let block = deflate(&mut z, &[]);
        let mut data = ZRLE.to_be_bytes().to_vec();
        data.extend_from_slice(&(block.len() as u32).to_be_bytes());
        data.extend_from_slice(&block);
        let Ok(Change::Cursor(c)) = d.rect(&mut io::Cursor::new(data), &mut fb, (0, 0, 0, 0), CURSOR_WITH_ALPHA) else {
            panic!("curseur caché")
        };
        assert_eq!((c.width, c.height), (0, 0));
        // Le format de l'écran est revenu.
        rect(&mut d, &mut fb, (0, 0, 1, 1), RAW, &[0, 0, 255, 0]);
        assert_eq!(fb.pixels[0], RED);

        let mut shown = Framebuffer::new(4, 4);
        fb.dirty = None;
        fb.fill(1, 1, 2, 1, RED);
        fb.flush_into(&mut shown);
        assert_eq!(shown.pixels[5], RED);
        assert_eq!(fb.dirty, None);
        assert_eq!(shown.dirty, Some([0, 0, 4, 4]));
    }

    #[test]
    fn lengths() {
        assert_eq!(compact_length(&mut io::Cursor::new(vec![0x05])).unwrap(), 5);
        assert_eq!(compact_length(&mut io::Cursor::new(vec![0x90, 0x4e])).unwrap(), 10000);
        assert_eq!(
            compact_length(&mut io::Cursor::new(vec![0xff, 0xff, 0xff])).unwrap(),
            4194303
        );
        assert_eq!(run_length(&mut io::Cursor::new(vec![255, 3])).unwrap(), 259);
    }
}
