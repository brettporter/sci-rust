use itertools::Itertools;

use crate::graphics::Colour;

pub struct Cel {
    size_x: u16,
    size_y: u16,
    x_pos: i8,
    y_pos: i8,
    alpha_key: u8,
    bitmap: Vec<u8>,
}

pub type View = Vec<Vec<Cel>>;

pub fn load_view(resource: &crate::resource::Resource) -> View {
    let data = &resource.resource_data;

    // TODO: make a resource reader to reduce boilerplate
    let num_groups = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
    // TODO: u16 - A bitmask containing the ’mirrored’ flag for each of the groups, with the LSB containing the ’mirrored’ flag for group 0
    let _mask = u16::from_le_bytes(data[2..4].try_into().unwrap());
    // skip unknown * 2
    assert!(num_groups <= 16);

    let group_offsets = (0..num_groups * 2)
        .step_by(2)
        .map(|g| u16::from_le_bytes(data[g + 8..g + 10].try_into().unwrap()) as usize);

    group_offsets
        .map(|group_offset| {
            let num_cels =
                u16::from_le_bytes(data[group_offset..group_offset + 2].try_into().unwrap())
                    as usize;
            // skip unknown
            let cel_offsets = (group_offset + 4..group_offset + num_cels * 2 + 4)
                .step_by(2)
                .map(|c| u16::from_le_bytes(data[c..c + 2].try_into().unwrap()) as usize);

            cel_offsets
                .map(|cel_offset| {
                    let size_x =
                        u16::from_le_bytes(data[cel_offset..cel_offset + 2].try_into().unwrap());
                    let size_y = u16::from_le_bytes(
                        data[cel_offset + 2..cel_offset + 4].try_into().unwrap(),
                    );
                    let x_pos = data[cel_offset + 4] as i8;
                    let y_pos = data[cel_offset + 5] as i8;
                    let alpha_key = data[cel_offset + 6];

                    let mut bitmap = Vec::new();
                    let mut offset = cel_offset + 7;
                    while bitmap.len() < size_x as usize * size_y as usize {
                        let colour = data[offset] & 0xF;
                        let rpt = data[offset] >> 4;
                        for _ in 0..rpt {
                            bitmap.push(colour);
                        }
                        offset += 1;
                    }

                    Cel {
                        size_x,
                        size_y,
                        x_pos,
                        y_pos,
                        alpha_key,
                        bitmap,
                    }
                })
                .collect_vec()
        })
        .collect_vec()
}

pub(crate) fn draw_image(
    view: &View,
    group: usize,
    cel_idx: usize,
    canvas: &mut crate::graphics::GraphicsContext,
) {
    let cel = &view[group][cel_idx];
    for y in 0..cel.size_y {
        for x in 0..cel.size_x {
            // TODO: just iterate
            let idx = y * cel.size_x + x;
            let c = cel.bitmap[idx as usize];
            if c != cel.alpha_key {
                // TODO: given repeats, probably no need to collect first - can just as easily draw here from the original bitmap
                let colour = Colour::from_ega(c);
                canvas.set_draw_color(colour.r, colour.g, colour.b); // TODO: palette needed?
                canvas.draw_point(
                    x as i32 + cel.x_pos as i32,
                    y as i32 + 200 - cel.size_y as i32 - cel.y_pos as i32,
                );
            }
        }
    }
}
