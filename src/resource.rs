use itertools::Itertools;

use std::{collections::HashMap, io, path::Path};

use num_traits::{FromPrimitive, ToPrimitive};

use decompress::CompressionType;

mod decompress;

#[derive(FromPrimitive, ToPrimitive, Debug, PartialEq)]
pub(crate) enum ResourceType {
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

struct ResourceMapEntry {
    resource_type: ResourceType,
    resource_number: u16,
    resource_file_number: u8,
    resource_file_offset: u32,
}

pub(crate) struct Resource {
    resource_type: ResourceType,
    resource_number: u16,

    // TODO: better way to handle this? For now just raw uncompressed data
    pub resource_data: Vec<u8>,
}

fn parse_resource_id(value: u16) -> (ResourceType, u16) {
    // TODO: handle errors in invalid resource types

    // 5 bit number of the resource type
    let resource_type: ResourceType = FromPrimitive::from_u16(value >> 11).unwrap();
    // 11 bit resource number
    let resource_number = value & 0x7ff;
    (resource_type, resource_number)
}

fn get_resource_key(resource_type: ResourceType, resource_number: u16) -> u16 {
    let t = ToPrimitive::to_u16(&resource_type).unwrap();
    t << 11 | resource_number
}

fn read_resource_map(path: &Path) -> Result<HashMap<u16, ResourceMapEntry>, io::Error> {
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

        let id = u16::from_le_bytes(rec[0..2].try_into().unwrap());
        let (resource_type, resource_number) = parse_resource_id(id);

        let value = u32::from_le_bytes(rec[2..6].try_into().unwrap());
        // 8 bit number of the resource file containing the resource
        let resource_file_number = (value >> 26) as u8;
        // 24 bit number of the byte offset of the resource within the resource file
        let resource_file_offset = value & 0x3ffffff;

        // Need to key by id because there will be some resources with the same number but different type
        entries.insert(
            id,
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

pub(crate) fn load_all_resources(game_path: &Path) -> Result<HashMap<u16, Resource>, io::Error> {
    let resource_map = read_resource_map(&game_path.join("RESOURCE.MAP").as_path())?;

    let lookup = resource_map
        .values()
        .into_group_map_by(|r| (*r).resource_file_number);

    let mut resources: HashMap<u16, Resource> = HashMap::new();
    for (file_number, resource_list) in lookup {
        // We have enough memory to load the whole resource file rather than seeking to the offset
        let buffer = std::fs::read(game_path.join(format!("RESOURCE.{:0>3}", file_number)))?;

        for entry in resource_list {
            // Read SCI0 resource
            let offset = entry.resource_file_offset as usize;
            let header = &buffer[offset..offset + 8];

            let id = u16::from_le_bytes(header[0..2].try_into().unwrap());
            let (resource_type, resource_number) = parse_resource_id(id);

            // TODO: shouldn't panic on bad input data
            assert_eq!(resource_type, entry.resource_type);
            assert_eq!(resource_number, entry.resource_number);

            let comp_size = u16::from_le_bytes(header[2..4].try_into().unwrap()) as usize;
            let decomp_size = u16::from_le_bytes(header[4..6].try_into().unwrap()) as usize;
            let method = u16::from_le_bytes(header[6..8].try_into().unwrap());

            // Comp-size starts after that field
            let compressed_data = buffer[offset + 8..offset + 4 + comp_size].to_vec();

            // TODO: refactor into the decompress module and let it handle different SCI versions
            let resource_data = match FromPrimitive::from_u16(method).unwrap() {
                CompressionType::None => compressed_data,
                CompressionType::LZW => vec![0; decomp_size], // TODO: this isn't correct but don't want to fail yet
                CompressionType::Huffman => decompress::huffman_decode(compressed_data),
            };

            assert_eq!(resource_data.len(), decomp_size);

            resources.insert(
                id,
                Resource {
                    resource_type,
                    resource_number,
                    resource_data,
                },
            );
        }
    }
    Ok(resources)
}

pub(crate) fn get_resource<'a>(
    resources: &'a HashMap<u16, Resource>,
    resource_type: ResourceType,
    resource_number: u16,
) -> Option<&'a Resource> {
    let id = get_resource_key(resource_type, resource_number);
    resources.get(&id)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    // This currently relies on the presence of game data from the Colonel's Bequest in the game_data/CB folder.
    // TODO: construct some relevant test data that can be used instead
    fn get_test_game_path() -> PathBuf {
        PathBuf::from("game_data/CB")
    }

    fn get_test_resource_map_path() -> PathBuf {
        get_test_game_path().join("RESOURCE.MAP")
    }

    #[test]
    fn it_correctly_reads_resource_map() {
        let entries = read_resource_map(get_test_resource_map_path().as_path())
            .expect("Resource map file is not present in game_data/CB/RESOURCE.MAP");

        let id = get_resource_key(ResourceType::Pic, 77);
        assert!(entries.contains_key(&id));
        let entry = entries.get(&id).unwrap();
        assert_eq!(entry.resource_type, ResourceType::Pic);
        assert_eq!(entry.resource_file_number, 10);
        assert_eq!(entry.resource_file_offset, 79925);
        assert_eq!(entry.resource_number, 77);
    }

    #[test]
    fn it_correctly_reads_resources() {
        let resources = load_all_resources(get_test_game_path().as_path())
            .expect("Resource files are present in game_data/CB");

        let resource = get_resource(&resources, ResourceType::Text, 409).unwrap();
        assert_eq!(resource.resource_number, 409);
        assert_eq!(resource.resource_type, ResourceType::Text);
        assert_eq!(
            resource.resource_data,
            "\nHave you previously attended a performance of\n\"The Colonel's Bequest?\"\n\n\0"
                .as_bytes()
        );

        // todo!("Test a resource with compression")
        // TODO: test a resource with compression
        // assert the result of the bytes with a hash
    }
}
