use std::{
    cell::Cell,
    hash::DefaultHasher,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use anyhow::Context;
use fimg::{pixels::convert::RGB, Image, Pack};
use resource::{resource, Resource};
use rusttype::{point, Point, PositionedGlyph, Rect, Scale};
use wasmtime::*;

use crate::ProgramOptions;

use super::{clone_c_string, JumpTableBuilder, MemoryExt, SdkState};

// MARK: Jump Table

pub fn build_display_jump_table(memory: Memory, builder: &mut JumpTableBuilder) {
    // vexDisplayForegroundColor
    builder.insert(0x640, move |mut caller: Caller<'_, SdkState>, col: u32| {
        caller.data_mut().display.foreground_color = RGB::unpack(col);
    });

    // vexDisplayBackgroundColor
    builder.insert(0x644, move |mut caller: Caller<'_, SdkState>, col: u32| {
        caller.data_mut().display.background_color = RGB::unpack(col);
    });

    // vexDisplayRectDraw
    builder.insert(
        0x668,
        move |mut caller: Caller<'_, SdkState>, x1: i32, y1: i32, x2: i32, y2: i32| {
            caller
                .data_mut()
                .display
                .draw(Path::Rect { x1, y1, x2, y2 }, true);
        },
    );

    // vexDisplayRectFill
    builder.insert(
        0x670,
        move |mut caller: Caller<'_, SdkState>, x1: i32, y1: i32, x2: i32, y2: i32| {
            caller
                .data_mut()
                .display
                .draw(Path::Rect { x1, y1, x2, y2 }, false);
        },
    );

    // vexDisplayCircleDraw
    builder.insert(
        0x674,
        move |mut caller: Caller<'_, SdkState>, cx: i32, cy: i32, radius: i32| {
            caller
                .data_mut()
                .display
                .draw(Path::Circle { cx, cy, radius }, true);
        },
    );

    // vexDisplayCircleFill
    builder.insert(
        0x67c,
        move |mut caller: Caller<'_, SdkState>, cx: i32, cy: i32, radius: i32| {
            caller
                .data_mut()
                .display
                .draw(Path::Circle { cx, cy, radius }, false);
        },
    );

    // vexDisplayStringWidthGet
    builder.insert(
        0x6c0,
        move |mut caller: Caller<'_, SdkState>, string_ptr: i32| {
            let string = clone_c_string!(string_ptr as usize, from caller using memory)?;
            let size = caller
                .data()
                .display
                .calculate_string_size(string, FontType::Normal);
            Ok(size.x as u32)
        },
    );

    // vexDisplayStringHeightGet
    builder.insert(
        0x6c4,
        move |mut caller: Caller<'_, SdkState>, string_ptr: i32| {
            let string = clone_c_string!(string_ptr as usize, from caller using memory)?;
            let size = caller
                .data()
                .display
                .calculate_string_size(string, FontType::Normal);
            Ok(size.y as u32)
        },
    );

    // vexDisplayRender
    builder.insert(
        0x7a0,
        move |mut caller: Caller<'_, SdkState>, _vsync_wait: i32, _run_scheduler: i32| {
            caller.data_mut().display.render(true);
        },
    );

    // vexDisplayDoubleBufferDisable
    builder.insert(0x7a4, move |mut caller: Caller<'_, SdkState>| {
        caller.data_mut().display.disable_double_buffer();
    });

    // vexDisplayVPrintf
    builder.insert(
        0x680,
        move |mut caller: Caller<'_, SdkState>,
              x_pos: i32,
              y_pos: i32,
              opaque: i32,
              format_ptr: u32,
              _args: u32|
              -> Result<()> {
            let format = clone_c_string!(format_ptr as usize, from caller using memory)?;
            caller.data_mut().display.write_text(
                format,
                (x_pos, y_pos),
                TextOptions {
                    transparent: opaque == 0,
                    ..Default::default()
                },
            );
            Ok(())
        },
    );

    // vexDisplayVString
    builder.insert(
        0x684,
        move |mut caller: Caller<'_, SdkState>,
              line_number: i32,
              format_ptr: u32,
              _args: u32|
              -> Result<()> {
            let format = clone_c_string!(format_ptr as usize, from caller using memory)?;
            caller.data_mut().display.write_text(
                format,
                TextLine(line_number).coords(),
                TextOptions::default(),
            );
            Ok(())
        },
    );

    // vexDisplayVSmallStringAt
    builder.insert(
        0x6b0,
        move |mut caller: Caller<'_, SdkState>,
              x_pos: i32,
              y_pos: i32,
              format_ptr: u32,
              _args: u32|
              -> Result<()> {
            let format = clone_c_string!(format_ptr as usize, from caller using memory)?;
            caller.data_mut().display.write_text(
                format,
                (x_pos, y_pos),
                TextOptions {
                    font_type: FontType::Small,
                    ..Default::default()
                },
            );
            Ok(())
        },
    );

    // vexDisplayCopyRect
    builder.insert(
        0x654,
        move |mut caller: Caller<'_, SdkState>,
              x1: i32,
              y1: i32,
              x2: i32,
              y2: i32,
              buffer_ptr: u32,
              stride: u32|
              -> Result<()> {
            let buffer_len = (x2 - x1) as usize * (y2 - y1) as usize * 4;
            let buffer = memory.data(&mut caller)[buffer_ptr as usize..][..buffer_len].to_vec();
            caller
                .data_mut()
                .display
                .draw_buffer(&buffer, (x1, y1), (x2, y2), stride);
            Ok(())
        },
    );
}

// MARK: API

pub enum Path {
    Rect { x1: i32, y1: i32, x2: i32, y2: i32 },
    Circle { cx: i32, cy: i32, radius: i32 },
}

impl From<Rect<i32>> for Path {
    fn from(rect: Rect<i32>) -> Self {
        Path::Rect {
            x1: rect.min.x,
            y1: rect.min.y,
            x2: rect.max.x,
            y2: rect.max.y,
        }
    }
}

impl Path {
    fn normalize(&mut self) {
        match self {
            Path::Rect { x1, y1, x2, y2 } => {
                *x1 = (*x1).clamp(0, DISPLAY_WIDTH as i32 - 1);
                *y1 = (*y1).clamp(0, DISPLAY_HEIGHT as i32 - 1);
                *x2 = (*x2).clamp(0, DISPLAY_WIDTH as i32 - 1);
                *y2 = (*y2).clamp(0, DISPLAY_HEIGHT as i32 - 1);
            }
            Path::Circle { cx, cy, .. } => {
                *cx = (*cx).clamp(0, DISPLAY_WIDTH as i32 - 1);
                *cy = (*cy).clamp(0, DISPLAY_HEIGHT as i32 - 1);
            }
        }
    }

    fn draw<T: AsMut<[u8]> + AsRef<[u8]>>(
        &self,
        canvas: &mut Image<T, 3>,
        stroke: bool,
        color: RGB,
    ) {
        match *self {
            Path::Rect { x1, y1, x2, y2 } => {
                let coords = (x1 as u32, y1 as u32);
                let width = (x2 - x1).try_into().unwrap();
                let height = (y2 - y1).try_into().unwrap();
                if stroke {
                    canvas.r#box(coords, width, height, color);
                } else {
                    canvas.filled_box(coords, width, height, color);
                }
            }
            Path::Circle { cx, cy, radius } => {
                if stroke {
                    canvas.border_circle((cx, cy), radius, color);
                } else {
                    canvas.circle((cx, cy), radius, color);
                }
            }
        }
    }
}

pub const DISPLAY_HEIGHT: u32 = 272;
pub const DISPLAY_WIDTH: u32 = 480;
pub const HEADER_HEIGHT: u32 = 32;

pub const BLACK: RGB = [0, 0, 0];
pub const WHITE: RGB = [255, 255, 255];
pub const HEADER_BG: RGB = [0x00, 0x99, 0xCC];

pub struct Display {
    pub foreground_color: RGB,
    pub background_color: RGB,
    pub canvas: Image<Box<[u8]>, 3>,
    mono_font: rusttype::Font<'static>,
    program_options: ProgramOptions,
    render_mode: RenderMode,
    text_layout_cache: Cell<Option<(String, FontType, Vec<PositionedGlyph<'static>>)>>,
}

impl Deref for Display {
    type Target = Image<Box<[u8]>, 3>;

    fn deref(&self) -> &Self::Target {
        &self.canvas
    }
}

impl DerefMut for Display {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.canvas
    }
}

impl Display {
    pub fn new(program_options: ProgramOptions) -> Self {
        let canvas =
            Image::build(DISPLAY_WIDTH, DISPLAY_HEIGHT).fill(program_options.default_bg_color());
        let font_bytes = resource!("/fonts/NotoMono-Regular.ttf");
        let mono_font = rusttype::Font::try_from_vec(font_bytes.to_vec()).unwrap();

        Self {
            foreground_color: program_options.default_fg_color(),
            background_color: program_options.default_bg_color(),
            mono_font,
            canvas,
            program_options,
            render_mode: RenderMode::default(),
            text_layout_cache: Cell::default(),
        }
    }

    fn draw_buffer(
        &mut self,
        buf: &[u8],
        top_left: (i32, i32),
        bot_right: (i32, i32),
        stride: u32,
    ) {
        let mut y = top_left.1;
        for row in buf.chunks((stride * 4) as usize) {
            if y > bot_right.1 {
                break;
            }

            let mut x = top_left.0;
            for pixel in row.chunks(4) {
                let color = RGB::unpack(u32::from_le_bytes(pixel[0..4].try_into().unwrap()));
                if x >= 0 && x < self.width() as i32 && y >= 0 && y < self.height() as i32 {
                    // I didn't see a safe version of this...?
                    // SAFETY: bounds are checked
                    unsafe { self.set_pixel(x.try_into().unwrap(), y.try_into().unwrap(), color) };
                }
                x += 1;
            }
            y += 1;
        }
    }

    /// Draws the blue program header at the top of the display.
    fn draw_header(&mut self) {
        self.filled_box((0, 0), DISPLAY_WIDTH, HEADER_HEIGHT, HEADER_BG);
    }

    /// Saves the display to a PNG file.
    pub fn render(&mut self, explicitly_requested: bool) {
        if explicitly_requested {
            self.render_mode = RenderMode::DoubleBuffered;
        } else if self.render_mode == RenderMode::DoubleBuffered {
            return;
        }

        self.draw_header();
        // self.flush();

        self.save("display.png");
    }

    pub fn disable_double_buffer(&mut self) {
        self.render_mode = RenderMode::Immediate;
    }

    /// Erases the display by filling it with the background color.
    pub fn erase(&mut self) {
        self.canvas.filled_box(
            (0, 0),
            DISPLAY_WIDTH,
            DISPLAY_HEIGHT,
            self.program_options.default_bg_color(),
        );
    }

    /// Draws or strokes a shape on the display, in the foreground color.
    pub fn draw(&mut self, mut shape: Path, stroke: bool) {
        shape.normalize();
        shape.draw(&mut self.canvas, stroke, self.foreground_color);
        self.render(false);
    }

    fn take_cached_glyphs_for(
        &self,
        text: &str,
        font_type: FontType,
    ) -> Option<Vec<PositionedGlyph<'static>>> {
        let (cached_text, cached_font, glyphs) = self.text_layout_cache.take()?;
        if text == cached_text && font_type == cached_font {
            Some(glyphs)
        } else {
            None
        }
    }

    fn glyphs_for(&self, text: &str, font_type: FontType) -> Vec<PositionedGlyph<'static>> {
        if let Some(glyphs) = self.take_cached_glyphs_for(text, font_type) {
            return glyphs;
        }

        let scale = Scale {
            y: font_type.font_size(),
            // V5's version of the Noto Mono font is slightly different
            // than the one bundled with the simulator, so we have to apply
            // an scale on the X axis and later move the characters further apart.
            x: font_type.font_size() * FontType::x_scale(),
        };
        let v_metrics = self.mono_font.v_metrics(scale);
        self.mono_font
            .layout(text, scale, point(0.0, 0.0 + v_metrics.ascent))
            .collect()
    }

    /// Calculates the shape of the area behind a text layout, so that it can be drawn on top of a background color.
    fn calculate_text_background(
        glyphs: &[PositionedGlyph],
        coords: (i32, i32),
        font_size: FontType,
    ) -> Option<Path> {
        let size = size_of_layout(glyphs)?;
        let mut backdrop = Path::Rect {
            x1: size.min.x + coords.0 - 1,
            y1: coords.1 + font_size.backdrop_y_offset(),
            x2: size.max.x + coords.0 + 1,
            y2: coords.1 + font_size.backdrop_y_offset() + font_size.line_height() - 1,
        };

        backdrop.normalize();
        Some(backdrop)
    }

    /// Writes text to the display at a given line number.
    ///
    /// # Arguments
    ///
    /// * `opaque`: Whether the text should be drawn on top of a background color.
    pub fn write_text(&mut self, text: String, mut coords: (i32, i32), options: TextOptions) {
        if text.is_empty() {
            return;
        }

        // The V5's text is all offset vertically from ours, so this adjustment makes it consistent.
        coords.1 += options.font_type.y_offset();

        let fg = self.foreground_color;
        let glyphs = self.glyphs_for(&text, options.font_type);

        if !options.transparent {
            let backdrop =
                Self::calculate_text_background(&glyphs, coords, options.font_type).unwrap();
            backdrop.draw(&mut self.canvas, false, self.background_color);
        }

        for (idx, glyph) in glyphs.iter().enumerate() {
            if let Some(bounding_box) = glyph.pixel_bounding_box() {
                // Draw the glyph into the image per-pixel
                glyph.draw(|mut x, mut y, alpha| {
                    // Apply offsets to make the coordinates image-relative, not text-relative
                    x += bounding_box.min.x as u32
                        + coords.0 as u32
                        // Similar reasoning to when we applied the x scale to the font.
                        + (FontType::x_spacing() * idx as f32) as u32;
                    y += bounding_box.min.y as u32 + coords.1 as u32;

                    if !(x < self.width() && y < self.height()) {
                        return;
                    }

                    // I didn't find a safe version of pixel and set_pixel.
                    // SAFETY: Pixel bounds are checked.
                    unsafe {
                        let old_pixel = self.pixel(x, y);

                        self.set_pixel(
                            x,
                            y,
                            // Taking this power seems to make the alpha blending look better;
                            // otherwise it's not heavy enough.
                            blend_pixel(old_pixel, fg, alpha.powf(0.4).clamp(0.0, 1.0)),
                        );
                    }
                });
            }
        }

        // Add (or re-add) the laid-out glyphs to the cache so they can be used later.
        self.text_layout_cache
            .set(Some((text, options.font_type, glyphs)));
        self.render(false);
    }

    pub fn calculate_string_size(&self, text: String, font_type: FontType) -> Point<i32> {
        let glyphs = self.glyphs_for(&text, font_type);
        let size = size_of_layout(&glyphs);
        self.text_layout_cache.set(Some((text, font_type, glyphs)));
        size.unwrap_or_default().max
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextLine(pub i32);

impl TextLine {
    pub fn coords(&self) -> (i32, i32) {
        (0, self.0 * 20 + HEADER_HEIGHT as i32)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FontType {
    Small,
    #[default]
    Normal,
    Big,
}

impl FontType {
    pub fn x_scale() -> f32 {
        0.9
    }

    pub fn x_spacing() -> f32 {
        1.1
    }

    pub fn font_size(&self) -> f32 {
        match self {
            FontType::Small => 15.0,
            FontType::Normal => 16.0,
            FontType::Big => 32.0,
        }
    }

    pub fn y_offset(&self) -> i32 {
        match self {
            FontType::Small => -2,
            FontType::Normal => -2,
            FontType::Big => -1,
        }
    }

    pub fn line_height(&self) -> i32 {
        match self {
            FontType::Small => 13,
            FontType::Normal => 2,
            FontType::Big => 2,
        }
    }

    pub fn backdrop_y_offset(&self) -> i32 {
        match self {
            FontType::Small => 2,
            FontType::Normal => 0,
            FontType::Big => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextOptions {
    pub transparent: bool,
    pub font_type: FontType,
}

/*pub trait Strokable {
    /// Creates a Rect that can be used to draw the border of `other`.
    fn stroking(other: Self) -> Self;
}

impl Strokable for Rect {
    fn stroking(mut other: Self) -> Self {
        other.x0 += 0.5;
        other.y0 += 0.5;
        other.x1 += 0.5;
        other.y1 += 0.5;
        other
    }
}

impl Strokable for Circle {
    fn stroking(mut other: Self) -> Self {
        other.radius += 0.5;
        other
    }
}*/

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RenderMode {
    #[default]
    Immediate,
    DoubleBuffered,
}

fn blend_pixel(bg: RGB, fg: RGB, fg_alpha: f32) -> RGB {
    // outputRed = (foregroundRed * foregroundAlpha) + (backgroundRed * (1.0 - foregroundAlpha));

    [
        (fg[0] as f32 * fg_alpha + bg[0] as f32 * (1.0 - fg_alpha)).round() as u8,
        (fg[1] as f32 * fg_alpha + bg[1] as f32 * (1.0 - fg_alpha)).round() as u8,
        (fg[2] as f32 * fg_alpha + bg[2] as f32 * (1.0 - fg_alpha)).round() as u8,
    ]
}

fn size_of_layout(glyphs: &[PositionedGlyph]) -> Option<Rect<i32>> {
    let last_char = glyphs.last()?;
    let first_char = &glyphs[0];
    let last_bounding_box = last_char.pixel_bounding_box().unwrap();
    let first_bounding_box = first_char.pixel_bounding_box().unwrap();
    Some(Rect {
        min: first_bounding_box.min,
        max: Point {
            x: last_bounding_box.max.x + (FontType::x_spacing() * glyphs.len() as f32) as i32,
            y: last_bounding_box.max.y,
        },
    })
}
