use std::collections::HashMap;

use log::debug;

use crate::{
    resource::{self, Resource, ResourceType},
    script::Script,
};

pub(crate) struct PMachine<'a> {
    resources: &'a HashMap<u16, Resource>,
    class_scripts: HashMap<u16, u16>,
    play_selector: u16,
}

struct ObjectInstance {
    func_selectors: HashMap<u16, (u16, u16)>,
}

const VOCAB_RESOURCE_CLASS_SCRIPTS: u16 = 996;
const VOCAB_RESOURCE_SELECTOR_NAMES: u16 = 997;
const VOCAB_RESOURCE_OPCODE_NAMES: u16 = 998;

const NO_SUPER_CLASS: u16 = 0xffff;

impl<'a> PMachine<'a> {
    pub(crate) fn init(
        resources: &'a std::collections::HashMap<u16, crate::resource::Resource>,
    ) -> Self {
        // TODO: these seem to be special cases, can we load them with the rest of the vocab resources?
        // TODO: class scripts may not be needed if we just load all the scripts and register classes? Currently supporting load on demand

        // Note that some of the classes and script numbers referenced in the vocab may not actually exist in the resources
        let class_scripts = load_vocab_class_scripts(
            resource::get_resource(
                &resources,
                ResourceType::Vocab,
                VOCAB_RESOURCE_CLASS_SCRIPTS,
            )
            .unwrap(),
        );

        // TODO: only needed for play method and debugging
        let selector_names = load_vocab_selector_names(
            resource::get_resource(
                &resources,
                ResourceType::Vocab,
                VOCAB_RESOURCE_SELECTOR_NAMES,
            )
            .unwrap(),
        );
        // TODO: handle missing method
        let (&play_selector, _) = selector_names
            .iter()
            .find(|(_, &name)| name == "play")
            .unwrap();

        // TODO: only needed for debugging
        load_vocab_opcode_names(
            resource::get_resource(&resources, ResourceType::Vocab, VOCAB_RESOURCE_OPCODE_NAMES)
                .unwrap(),
        );

        // TODO: initialise machine state and save for restarting the game
        // TODO: allocate machine stack

        PMachine {
            resources: &resources,
            class_scripts,
            play_selector,
        }
    }

    fn load_game_object(&self) -> ObjectInstance {
        // load the play method from the game object (at exports[0]) in script 000
        let init_script =
            Script::load(resource::get_resource(&self.resources, ResourceType::Script, 0).unwrap());

        self.initialise_object(init_script.get_main_object())
    }

    fn initialise_object(&self, obj: &crate::script::ClassDefinition) -> ObjectInstance {
        let mut func_selectors: HashMap<u16, (u16, u16)> = HashMap::new();

        for (s, offset) in &obj.function_selectors {
            func_selectors.insert(*s, (obj.script_number, *offset));
        }

        if obj.super_class != NO_SUPER_CLASS {
            let script_num = self.class_scripts[&obj.super_class];

            // TODO: Store loaded script in a map - cache
            let script = Script::load(
                resource::get_resource(&self.resources, ResourceType::Script, script_num).unwrap(),
            );

            let super_class_def = script.get_class(obj.super_class);
            let instance = self.initialise_object(super_class_def);
            func_selectors.extend(instance.func_selectors);
        }

        ObjectInstance { func_selectors }
    }

    pub(crate) fn run_game_play_method(&self) {
        let game_object = self.load_game_object();

        let (script_number, code_offset) =
            game_object.func_selectors.get(&self.play_selector).unwrap();

        debug!(
            "Found play {} with code offset {:x?} in script {}",
            self.play_selector, code_offset, script_number
        );

        // TODO: should not be reloading this again
        let script = Script::load(
            resource::get_resource(&self.resources, ResourceType::Script, *script_number).unwrap(),
        );

        // TODO: this effectively becomes the main loop
        // TODO: run the code
    }
}

fn load_vocab_selector_names(resource: &Resource) -> HashMap<u16, &str> {
    let data = resource.resource_data.as_slice();

    // TODO: convenience methods for u16 here, maybe on resource?
    let count = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;

    // Note off by one error in count
    HashMap::from_iter((0..=count).map(|i| {
        let sel_offset = i * 2 + 2;
        let offset =
            u16::from_le_bytes(data[sel_offset..sel_offset + 2].try_into().unwrap()) as usize;

        let len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        let s = std::str::from_utf8(&data[offset + 2..offset + len + 2]).unwrap();

        debug!("Found selector {} ({i})", s);

        (i as u16, s)
    }))
}

fn load_vocab_opcode_names(resource: &Resource) {
    let data = resource.resource_data.as_slice();

    let count = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;

    for i in 0..count {
        let opcode_offset = i * 2 + 2;
        let offset =
            u16::from_le_bytes(data[opcode_offset..opcode_offset + 2].try_into().unwrap()) as usize;

        let len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize - 2;
        let t = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap()) as usize;
        let s = std::str::from_utf8(&data[offset + 4..offset + len + 4]).unwrap();

        debug!("Found opcode type {}, name {} ({:x})", t, s, i << 1);
    }
}

fn load_vocab_class_scripts(resource: &Resource) -> HashMap<u16, u16> {
    let data = resource.resource_data.as_slice();
    HashMap::from_iter((0..data.len() / 4).map(|i| {
        let offset = i * 4;
        let segment = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
        let script_num = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap());
        assert_eq!(segment, 0);

        debug!(
            "Class {} in script {} with segment {}",
            i, script_num, segment
        );

        (i as u16, script_num)
    }))
}
