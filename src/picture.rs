use std::io::{Cursor, SeekFrom};

use bitstream_io::{BigEndian, BitRead, BitReader};

use num_traits::FromPrimitive;

use crate::{
    graphics::{Graphics, GraphicsContext},
    resource::Resource,
};

#[derive(FromPrimitive)]
#[repr(u8)]
enum PictureCommand {
    SetVisualColour = 0xf0,
    DisableVisual,
    SetPriorityColour,
    DisablePriority,
    DrawShortRelativePatterns,
    DrawRelativeLines,
    DrawAbsoluteLines,
    DrawShortRelativeLines,
    FloodFill,
    SetPattern,
    DrawAbsolutePatterns,
    SetControlColour,
    DisableControl,
    DrawRelativePatterns,
    ExtendedCommand,
    Finish,
}

#[derive(FromPrimitive)]
#[repr(u8)]
enum ExtendedCommand {
    SetPaletteEntry,
    SetPalette,
    SetMonoPalette,
    SetVisualMono,
    DisableVisualMono,
    SetDirectMonoVisual,
    DisableDirectMonoVisual,
}

enum BrushShape {
    CIRCLE,
    RECTANGLE,
}

struct PatternBrush {
    size: i32,
    shape: BrushShape,
    use_texture: bool,
}

struct Point {
    x: i32,
    y: i32,
}
impl Point {
    fn offset(&self, x: i32, y: i32) -> Point {
        Point {
            x: self.x + x,
            y: self.y + y,
        }
    }
}

#[derive(Clone)]
struct Colour {
    r: u8,
    g: u8,
    b: u8,
}
impl Colour {
    // See https://en.wikipedia.org/wiki/Enhanced_Graphics_Adapter
    const EGA_COLOURS: [[u8; 3]; 16] = [
        [0, 0, 0],       // Black
        [0, 0, 170],     // Blue
        [0, 170, 0],     // Green
        [0, 170, 170],   // Cyan
        [170, 0, 0],     // Red
        [170, 0, 170],   // Magenta
        [170, 85, 0],    // Brown
        [170, 170, 170], // Light grey
        [85, 85, 85],    // Dark grey
        [85, 85, 255],   // Bright blue
        [85, 255, 85],   // Bright green
        [85, 255, 255],  // Bright cyan
        [255, 85, 85],   // Bright red
        [255, 85, 255],  // Bright magenta
        [255, 255, 85],  // Bright yellow
        [255, 255, 255], // White
    ];

    fn from_ega(c: u8) -> Self {
        Self {
            r: Self::EGA_COLOURS[c as usize][0],
            g: Self::EGA_COLOURS[c as usize][1],
            b: Self::EGA_COLOURS[c as usize][2],
        }
    }
}
type Palette = [DitheredColour; PALETTE_SIZE];
const PALETTE_SIZE: usize = 40;

#[derive(Clone)]
struct DitheredColour {
    c1: Colour,
    c2: Colour,
}
impl DitheredColour {
    fn blend(&self) -> Colour {
        Colour {
            r: ((self.c1.r as u16 + self.c2.r as u16) / 2) as u8,
            g: ((self.c1.g as u16 + self.c2.g as u16) / 2) as u8,
            b: ((self.c1.b as u16 + self.c2.b as u16) / 2) as u8,
        }
    }

    fn from_ega(c1: u8, c2: u8) -> Self {
        Self {
            c1: Colour::from_ega(c1),
            c2: Colour::from_ega(c2),
        }
    }

    fn default_palette() -> Palette {
        [
            DitheredColour::from_ega(0x0, 0x0),
            DitheredColour::from_ega(0x1, 0x1),
            DitheredColour::from_ega(0x2, 0x2),
            DitheredColour::from_ega(0x3, 0x3),
            DitheredColour::from_ega(0x4, 0x4),
            DitheredColour::from_ega(0x5, 0x5),
            DitheredColour::from_ega(0x6, 0x6),
            DitheredColour::from_ega(0x7, 0x7),
            DitheredColour::from_ega(0x8, 0x8),
            DitheredColour::from_ega(0x9, 0x9),
            DitheredColour::from_ega(0xa, 0xa),
            DitheredColour::from_ega(0xb, 0xb),
            DitheredColour::from_ega(0xc, 0xc),
            DitheredColour::from_ega(0xd, 0xd),
            DitheredColour::from_ega(0xe, 0xe),
            DitheredColour::from_ega(0x8, 0x8),
            DitheredColour::from_ega(0x8, 0x8),
            DitheredColour::from_ega(0x0, 0x1),
            DitheredColour::from_ega(0x0, 0x2),
            DitheredColour::from_ega(0x0, 0x3),
            DitheredColour::from_ega(0x0, 0x4),
            DitheredColour::from_ega(0x0, 0x5),
            DitheredColour::from_ega(0x0, 0x6),
            DitheredColour::from_ega(0x8, 0x8),
            DitheredColour::from_ega(0x8, 0x8),
            DitheredColour::from_ega(0xf, 0x9),
            DitheredColour::from_ega(0xf, 0xa),
            DitheredColour::from_ega(0xf, 0xb),
            DitheredColour::from_ega(0xf, 0xc),
            DitheredColour::from_ega(0xf, 0xd),
            DitheredColour::from_ega(0xf, 0xe),
            DitheredColour::from_ega(0xf, 0xf),
            DitheredColour::from_ega(0x0, 0x8),
            DitheredColour::from_ega(0x9, 0x1),
            DitheredColour::from_ega(0x2, 0xa),
            DitheredColour::from_ega(0x3, 0xb),
            DitheredColour::from_ega(0x4, 0xc),
            DitheredColour::from_ega(0x5, 0xd),
            DitheredColour::from_ega(0x6, 0xe),
            DitheredColour::from_ega(0x8, 0x8),
        ]
    }
}

struct Deserializer<'a> {
    index: usize,
    data: &'a [u8],
}

impl<'a> Deserializer<'a> {
    fn new(resource: &'a Resource) -> Self {
        Self {
            index: 0,
            data: &resource.resource_data,
        }
    }

    fn read_byte(&mut self) -> u8 {
        let v = self.data[self.index];
        self.index += 1;
        v
    }

    fn read_command(&mut self) -> PictureCommand {
        // TODO: handle unknown type
        FromPrimitive::from_u8(self.read_byte()).unwrap()
    }

    fn read_extended_command(&mut self) -> ExtendedCommand {
        // TODO: handle unknown type
        FromPrimitive::from_u8(self.read_byte()).unwrap()
    }

    fn next_is_command(&self) -> bool {
        self.data[self.index] >= 0xF0
    }

    fn read_texture(&mut self) -> u8 {
        self.read_byte()
    }

    fn read_pattern_brush(&mut self) -> PatternBrush {
        let p = self.read_byte();
        PatternBrush {
            shape: if (p & 0b10000) != 0 {
                BrushShape::RECTANGLE
            } else {
                BrushShape::CIRCLE
            },
            use_texture: (p & 0b100000) != 0,
            size: (p & 0x7) as i32,
        }
    }

    fn read_palette_entry(&mut self) -> (usize, usize) {
        let v = self.read_byte() as usize;
        (v / PALETTE_SIZE, v % PALETTE_SIZE)
    }

    fn read_colour_pair(&mut self) -> DitheredColour {
        let c = self.read_byte();
        DitheredColour::from_ega(c >> 4, c & 0xF)
    }

    fn read_coordinates(&mut self) -> Point {
        let (upper, lower_x, lower_y) = (
            self.read_byte() as i32,
            self.read_byte() as i32,
            self.read_byte() as i32,
        );
        Point {
            x: ((upper & 0xF0) << 4) | lower_x,
            y: ((upper & 0x0F) << 8) | lower_y,
        }
    }

    fn read_relative_offset(&mut self) -> (i32, i32) {
        let value_y = self.read_byte() as i32;
        let value_x = self.read_byte() as i32;

        let offset_y = if value_y >= 128 {
            128 - value_y
        } else {
            value_y
        };
        let offset_x = if value_x >= 128 {
            value_x - 256
        } else {
            value_x
        };

        (offset_x, offset_y)
    }

    fn read_short_relative_offset(&mut self) -> (i32, i32) {
        let v = self.read_byte() as i32;
        let (x, y) = (v >> 4, v & 0xF);
        let offset_x = if x & 0x8 != 0 { -(x & 0x7) } else { x };
        let offset_y = if y & 0x8 != 0 { -(y & 0x7) } else { y };

        (offset_x, offset_y)
    }

    fn skip(&mut self, num: usize) {
        self.index += num;
    }
}

pub(crate) fn draw_image(graphics: &mut GraphicsContext, resource: &Resource) {
    let mut data = Deserializer::new(resource);

    // TODO: refactor state as we introduce control / priority drawing as well
    let mut visual = true;

    let mut palette = [
        DitheredColour::default_palette(),
        DitheredColour::default_palette(),
        DitheredColour::default_palette(),
        DitheredColour::default_palette(),
    ];

    let mut pattern_brush = PatternBrush {
        size: 0,
        shape: BrushShape::CIRCLE,
        use_texture: false,
    };

    loop {
        match data.read_command() {
            PictureCommand::SetVisualColour => {
                let (palette_num, palette_idx) = data.read_palette_entry();
                let dither = &palette[palette_num][palette_idx];
                // TODO: support dithering?
                let c = dither.blend();
                // TODO: currently just passing through to SDL but may be something we keep state on in here
                graphics.set_draw_color(c.r, c.g, c.b);
                visual = true;
            }
            PictureCommand::DisableVisual => {
                visual = false;
            }
            PictureCommand::SetPriorityColour => {
                // TODO: set priority colour
                data.skip(1);
            }
            PictureCommand::DisablePriority => {
                // TODO: disable priority
            }
            PictureCommand::DrawShortRelativePatterns => {
                let texture = pattern_brush.use_texture.then(|| data.read_texture());

                let mut p = data.read_coordinates();
                if visual {
                    draw_pattern(graphics, &p, &pattern_brush, texture);
                }

                while !data.next_is_command() {
                    let texture = pattern_brush.use_texture.then(|| data.read_texture());
                    let (offset_x, offset_y) = data.read_short_relative_offset();
                    p = p.offset(offset_x, offset_y);

                    if visual {
                        draw_pattern(graphics, &p, &pattern_brush, texture);
                    }
                }
            }
            PictureCommand::DrawRelativeLines => {
                let mut start = data.read_coordinates();

                while !data.next_is_command() {
                    let (offset_x, offset_y) = data.read_relative_offset();
                    let end = start.offset(offset_x, offset_y);

                    if visual {
                        draw_line(graphics, &start, &end);
                    }

                    start = end;
                }
            }
            PictureCommand::DrawAbsoluteLines => {
                let mut start = data.read_coordinates();

                while !data.next_is_command() {
                    let end = data.read_coordinates();

                    if visual {
                        draw_line(graphics, &start, &end);
                    }

                    start = end;
                }
            }
            PictureCommand::DrawShortRelativeLines => {
                let mut start = data.read_coordinates();

                while !data.next_is_command() {
                    let (offset_x, offset_y) = data.read_short_relative_offset();
                    let end = start.offset(offset_x, offset_y);

                    if visual {
                        draw_line(graphics, &start, &end);
                    }

                    start = end;
                }
            }
            PictureCommand::FloodFill => {
                while !data.next_is_command() {
                    let p = data.read_coordinates();

                    if visual {
                        graphics.flood_fill(p.x, p.y);
                    }
                }
            }
            PictureCommand::SetPattern => {
                pattern_brush = data.read_pattern_brush();
            }
            PictureCommand::DrawAbsolutePatterns => {
                while !data.next_is_command() {
                    let texture = pattern_brush.use_texture.then(|| data.read_texture());
                    let p = data.read_coordinates();
                    if visual {
                        draw_pattern(graphics, &p, &pattern_brush, texture);
                    }
                }
            }
            PictureCommand::SetControlColour => {
                // TODO - Set control colour
                data.skip(1);
            }
            PictureCommand::DisableControl => {
                // TODO - Disable control
            }
            PictureCommand::DrawRelativePatterns => {
                let texture = pattern_brush.use_texture.then(|| data.read_texture());

                let mut p = data.read_coordinates();
                if visual {
                    draw_pattern(graphics, &p, &pattern_brush, texture);
                }

                while !data.next_is_command() {
                    let texture = pattern_brush.use_texture.then(|| data.read_texture());
                    let (offset_x, offset_y) = data.read_relative_offset();
                    p = p.offset(offset_x, offset_y);

                    draw_pattern(graphics, &p, &pattern_brush, texture);
                }
            }
            PictureCommand::ExtendedCommand => {
                match data.read_extended_command() {
                    ExtendedCommand::SetPaletteEntry => {
                        while !data.next_is_command() {
                            let (palette_num, palette_idx) = data.read_palette_entry();
                            palette[palette_num][palette_idx] = data.read_colour_pair();
                        }
                    }
                    ExtendedCommand::SetPalette => {
                        let (_, palette_num) = data.read_palette_entry();
                        for entry in &mut palette[palette_num] {
                            *entry = data.read_colour_pair();
                        }
                    }
                    ExtendedCommand::SetMonoPalette => {
                        // TODO - mono palette (can ignore?)
                        data.skip(PALETTE_SIZE + 1);
                    }
                    ExtendedCommand::SetVisualMono => {
                        // TODO - mono set visual (can ignore?)
                        data.skip(1);
                    }
                    ExtendedCommand::DisableVisualMono => {
                        // TODO - mono disable visual (can ignore?)
                    }
                    ExtendedCommand::SetDirectMonoVisual => {
                        // TODO - mono set visual (no palette) (can ignore?)
                        data.skip(1);
                    }
                    ExtendedCommand::DisableDirectMonoVisual => {
                        // TODO - mono disable visual (no palette) (can ignore?)
                    }
                }
            }
            PictureCommand::Finish => return,
        }
    }
}

fn draw_pattern(
    graphics: &mut GraphicsContext,
    p: &Point,
    pattern_brush: &PatternBrush,
    texture: Option<u8>,
) {
    // If we exceed boundary actually move the brush rather than clip
    // TODO: we should get the actual viewport not a constant
    let size = pattern_brush.size;
    let x = p.x.max(size).min(Graphics::VIEWPORT_WIDTH - size - 1);
    let y = p.y.max(size).min(Graphics::VIEWPORT_HEIGHT - size - 1);

    match pattern_brush.shape {
        BrushShape::RECTANGLE => draw_pattern_rect(graphics, x, y, pattern_brush, texture),
        BrushShape::CIRCLE => draw_pattern_circle(graphics, x, y, pattern_brush, texture),
    }
}

fn draw_pattern_circle(
    graphics: &mut GraphicsContext,
    x: i32,
    y: i32,
    pattern_brush: &PatternBrush,
    texture: Option<u8>,
) {
    // These are padded out with zeros to allow a const array of fixed size, but not all bytes are used
    const PATTERN_WIDTHS: [[i32; 15]; 8] = [
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [1, 2, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [1, 2, 3, 3, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        [1, 3, 4, 4, 4, 4, 4, 3, 1, 0, 0, 0, 0, 0, 0],
        [1, 3, 4, 4, 5, 5, 5, 4, 4, 3, 2, 0, 0, 0, 0], // TODO: not sure this is right at top or bottom
        [2, 4, 5, 5, 6, 6, 6, 6, 6, 5, 5, 4, 2, 0, 0],
        [2, 4, 5, 6, 6, 7, 7, 7, 7, 7, 6, 6, 5, 4, 2],
    ];

    let size = pattern_brush.size;
    let widths = &PATTERN_WIDTHS[size as usize];
    assert_eq!(size, *widths.iter().max().unwrap());

    let mut reader = get_texture_reader(texture);
    for y_idx in 0..=size * 2 {
        let w = widths[y_idx as usize];
        for x_offset in -w..=w {
            // the interpreter wrapped at 255 not 256 bits
            if reader.position_in_bits().unwrap() == 255 {
                reader.seek_bits(SeekFrom::Start(0)).unwrap();
            }
            if !pattern_brush.use_texture || reader.read_bit().unwrap() {
                graphics.draw_point(x + x_offset, y + y_idx - size)
            }
        }
    }
}

fn draw_pattern_rect(
    graphics: &mut GraphicsContext,
    x: i32,
    y: i32,
    pattern_brush: &PatternBrush,
    texture: Option<u8>,
) {
    let size = pattern_brush.size;
    let mut reader = get_texture_reader(texture);
    for dy in -size..=size {
        for dx in -size..=size + 1 {
            // the interpreter wrapped at 255 not 256 bits
            if reader.position_in_bits().unwrap() == 255 {
                reader.seek_bits(SeekFrom::Start(0)).unwrap();
            }
            if !pattern_brush.use_texture || reader.read_bit().unwrap() {
                graphics.draw_point(x + dx, y + dy);
            }
        }
    }
}

fn get_texture_reader(texture: Option<u8>) -> BitReader<Cursor<[u8; 32]>, BigEndian> {
    const TEXTURE_DATA: [u8; 32] = [
        0x20, 0x94, 0x02, 0x24, 0x90, 0x82, 0xa4, 0xa2, 0x82, 0x09, 0x0a, 0x22, 0x12, 0x10, 0x42,
        0x14, 0x91, 0x4a, 0x91, 0x11, 0x08, 0x12, 0x25, 0x10, 0x22, 0xa8, 0x14, 0x24, 0x00, 0x50,
        0x24, 0x04,
    ];

    const PATTERN_MAPPING: [u32; 120] = [
        0x00, 0x18, 0x30, 0xc4, 0xdc, 0x65, 0xeb, 0x48, 0x60, 0xbd, 0x89, 0x04, 0x0a, 0xf4, 0x7d,
        0x6d, 0x85, 0xb0, 0x8e, 0x95, 0x1f, 0x22, 0x0d, 0xdf, 0x2a, 0x78, 0xd5, 0x73, 0x1c, 0xb4,
        0x40, 0xa1, 0xb9, 0x3c, 0xca, 0x58, 0x92, 0x34, 0xcc, 0xce, 0xd7, 0x42, 0x90, 0x0f, 0x8b,
        0x7f, 0x32, 0xed, 0x5c, 0x9d, 0xc8, 0x99, 0xad, 0x4e, 0x56, 0xa6, 0xf7, 0x68, 0xb7, 0x25,
        0x82, 0x37, 0x3a, 0x51, 0x69, 0x26, 0x38, 0x52, 0x9e, 0x9a, 0x4f, 0xa7, 0x43, 0x10, 0x80,
        0xee, 0x3d, 0x59, 0x35, 0xcf, 0x79, 0x74, 0xb5, 0xa2, 0xb1, 0x96, 0x23, 0xe0, 0xbe, 0x05,
        0xf5, 0x6e, 0x19, 0xc5, 0x66, 0x49, 0xf0, 0xd1, 0x54, 0xa9, 0x70, 0x4b, 0xa4, 0xe2, 0xe6,
        0xe5, 0xab, 0xe4, 0xd2, 0xaa, 0x4c, 0xe3, 0x06, 0x6f, 0xc6, 0x4a, 0x75, 0xa3, 0x97, 0xe1,
    ];

    // TODO: Still creating a reader if it's not used, not sure if there's a cleaner way without repeating more code
    let mut reader = BitReader::endian(Cursor::new(TEXTURE_DATA), BigEndian);
    if let Some(texture_number) = texture {
        // TODO: not sure why this is shifted left, we have some with 1 and some with 0 at LSB
        let mapping_idx = texture_number >> 1;
        let offset = PATTERN_MAPPING[mapping_idx as usize];
        reader.skip(offset).unwrap();
    }
    reader
}

// TODO: move to graphics?
fn bresenham<F>(start: &Point, end: &Point, f: &mut F)
where
    F: FnMut(i32, i32),
{
    let dx = (end.x - start.x).abs();
    let step_x = if start.x < end.x { 1 } else { -1 };
    let dy = -(end.y - start.y).abs();
    let step_y = if start.y < end.y { 1 } else { -1 };

    let mut err = dx + dy;
    let mut x = start.x;
    let mut y = start.y;

    loop {
        f(x, y);

        if x == end.x && y == end.y {
            break;
        }

        let e2 = err * 2;
        if e2 >= dy {
            err += dy;
            x += step_x;
        }
        if e2 <= dx {
            err += dx;
            y += step_y;
        }
    }
}

// TODO: move to graphics?
fn draw_line(graphics: &mut GraphicsContext, start: &Point, end: &Point) {
    // TODO: can we just use SDL instead?
    // canvas.draw_line(sdl2::rect::Point::new(start.x, start.y), sdl2::rect::Point::new(end.x, end.y))
    bresenham(start, end, &mut |x, y| graphics.draw_point(x, y))
}
