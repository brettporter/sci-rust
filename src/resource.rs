use std::{collections::HashMap, io, path::Path};

use num_traits::FromPrimitive;

mod decompress;

#[derive(FromPrimitive, Debug, PartialEq)]
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

pub(crate) fn read_resource_map(path: &Path) -> Result<HashMap<u16, ResourceMapEntry>, io::Error> {
    // TODO: Integration tests for this using game data that isn't checked in

    // Read SCI0 resource.map file

    let buffer = std::fs::read(path)?;
    let mut idx = 0;
    let mut entries: HashMap<u16, ResourceMapEntry> = HashMap::new();

    loop {
        let rec = &buffer[idx..idx + 6];

        // 0xffff ffff ffff marks the end of the resource map
        // These should be the last 6 bytes of the file
        if rec.iter().all(|&x| x == 0xff) {
            break;
        }

        // TODO: handle errors in invalid resource types

        let value = u16::from_le_bytes(rec[0..2].try_into().unwrap());
        // 5 bit number of the resource type
        let resource_type: ResourceType = FromPrimitive::from_u16(value >> 11).unwrap();
        // 11 bit resource number
        let resource_number = value & 0x7ff;

        let value = u32::from_le_bytes(rec[2..6].try_into().unwrap());
        // 8 bit number of the resource file containing the resource
        let resource_file_number = (value >> 26) as u8;
        // 24 bit number of the byte offset of the resource within the resource file
        let resource_file_offset = value & 0x3ffffff;

        entries.insert(
            resource_number,
            ResourceMapEntry {
                resource_type,
                resource_number,
                resource_file_number,
                resource_file_offset,
            },
        );

        idx += 6;
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_correctly_reads_resource_map() {
        // This currently relies on the presence of game data from the Colonel's Bequest in the game_data/CB folder.
        // TODO: construct some relevant test data that can be used instead

        let entries = read_resource_map(Path::new("game_data/CB/RESOURCE.MAP"))
            .expect("Resource map file is not present in game_data/CB/RESOURCE.MAP");

        assert!(entries.contains_key(&77));
        let entry = entries.get(&77).unwrap();
        assert_eq!(entry.resource_type, ResourceType::Pic);
        assert_eq!(entry.resource_file_number, 10);
        assert_eq!(entry.resource_file_offset, 79925);
        assert_eq!(entry.resource_number, 77);
    }
}
