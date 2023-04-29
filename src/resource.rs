use std::{io, path::Path};

use num_traits::FromPrimitive;

mod decompress;

#[derive(FromPrimitive, Debug)]
enum ResourceType {
    View,
    Pic,
    Script,
    Text,
    Sound,
    Memory,
    Vocab,
    Font,
    Cursor,
    Patch,
    Bitmap,
    Palette,
    // TODO: add other types later
}

pub(crate) struct ResourceMapEntry {
    resource_type: ResourceType,
    resource_number: u16,
    resource_file_number: u8,
    resource_file_offset: u32,
}

pub(crate) fn read_resource_map(path: &Path) -> Result<Vec<ResourceMapEntry>, io::Error> {
    // TODO: Integration tests for this using game data that isn't checked in

    // Read SCI0 resource.map file

    let buffer = std::fs::read(path)?;
    let mut idx = 0;
    let mut entries: Vec<ResourceMapEntry> = Vec::new();

    loop {
        let rec = &buffer[idx..idx + 6];
        if rec.iter().all(|&x| x == 0xff) {
            break;
        }

        // TODO: handle errors in unwrap
        let first = u16::from_le_bytes(rec[0..2].try_into().unwrap());
        let resource_type: ResourceType = FromPrimitive::from_u16(first >> 11).unwrap();
        let resource_number = first & 0x7ff;

        let second: u32 = u32::from_le_bytes(rec[2..6].try_into().unwrap());
        let resource_file_number = (second >> 26) as u8;
        let resource_file_offset = second & 0x3ffffff;

        entries.push(ResourceMapEntry {
            resource_type,
            resource_number,
            resource_file_number,
            resource_file_offset,
        });

        idx += 6;
    }

    Ok(entries)
}
