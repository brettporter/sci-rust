mod decompress;

enum ResourceType {
    View = 0,
    Pic = 1,
    Script = 2,
    Text = 3,
    // Start simple, other types later
}

struct ResourceMapEntry {
    resourceType: ResourceType,
    resourceNumber: u16,
    resourceFileNumber: u8,
    resourceFileOffset: u32,
}

struct ResourceMap {
    entries: Vec<ResourceMapEntry>,
}

fn read_resource_map() -> ResourceMap {
    // TODO: Integration tests for this using game data that isn't checked in

    ResourceMap { entries: vec![] }
}
