use std::collections::HashMap;

use elsa::FrozenMap;
use log::debug;

use crate::{
    resource::{self, Resource, ResourceType},
    script::Script,
};

pub(crate) struct PMachine<'a> {
    resources: &'a HashMap<u16, Resource>,
    class_scripts: HashMap<u16, u16>,
    play_selector: u16,
    script_cache: FrozenMap<u16, Box<Script<'a>>>,
}

enum VariableType {
    Global,
    Local,
    Temporary,
    Paramter,
}

struct MachineState<'a> {
    ax: u16,                  // accumulator
    ip: usize,                // instruction pointer
    current_obj: Option<u16>, // current object unique identifier
    // TODO: should this just be current_obj and ip offset modified
    current_data: &'a [u8], // currently executing script data
    stack: Vec<u16>,        // stack
    var: [u16; 4],          // variable points for each type (global, local, temporary, param)
}
impl MachineState<'_> {
    fn read_byte(&mut self) -> u8 {
        let v = self.current_data[self.ip];
        self.ip += 1;
        v
    }

    fn read_word(&mut self) -> u16 {
        let v = u16::from_le_bytes(self.current_data[self.ip..self.ip + 2].try_into().unwrap());
        self.ip += 2;
        v
    }

    fn jump(&mut self, pos: u16) {
        self.ip += pos as usize;
    }
}

struct ObjectInstance {
    id: u16,
    func_selectors: HashMap<u16, (u16, u16)>,
}

const SCRIPT_MAIN: u16 = 000;

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
        PMachine {
            resources: &resources,
            class_scripts,
            play_selector,
            script_cache: FrozenMap::new(),
        }
    }

    fn load_game_object(&self) -> ObjectInstance {
        // load the play method from the game object (at exports[0]) in script 000
        let init_script = self.load_script(SCRIPT_MAIN);

        self.initialise_object(init_script.get_main_object())
    }

    fn initialise_object(&self, obj: &crate::script::ObjectDefinition) -> ObjectInstance {
        ObjectInstance {
            id: obj.id, // TODO: not currently globally unique
            func_selectors: self.get_inherited_functions(&obj.class_definition),
        }
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
        let script = self.load_script(*script_number);

        // TODO: this effectively becomes the main loop - do we need to pass in event handling? have an exit?

        // TODO: better to do this by execution a machine function?

        let mut state = MachineState {
            current_obj: Some(game_object.id),
            current_data: script.data,
            ip: *code_offset as usize,
            ax: 0,
            stack: Vec::new(),
            var: [0; 4],
        };

        self.run(&mut state);
    }

    fn run(&self, state: &mut MachineState) {
        let current_obj = state.current_obj;

        // TODO: do we need the ones in the state or not?
        let mut ax = 0;
        let mut stack = Vec::new();

        // TODO: get from all loaded scripts, rather than loading again
        let script = self.load_script(SCRIPT_MAIN);
        let mut global_vars: Vec<u16> = script.variables.clone();

        loop {
            let cmd = state.read_byte();
            debug!("Executing {:x}", cmd);
            // TODO: do we do constants for opcodes? Do we enumberate the B / W variants or add tooling for this?
            match cmd {
                0x18 | 0x19 => {
                    // not
                    ax = if ax == 0 { 1 } else { 0 };
                }
                0x30 => {
                    // bnt W
                    let pos = state.read_word();
                    if ax == 0 {
                        state.jump(pos);
                    }
                }
                0x38 => {
                    // pushi W
                    stack.push(state.read_word());
                }
                0x39 => {
                    // pushi B
                    stack.push(state.read_byte() as u16); // TODO: is this right or will we rely on the stack pointer knowing the byte size?
                }
                0x43 => {
                    // callk B
                    let k_func = state.read_byte();
                    let k_params = state.read_byte();

                    // TODO:
                    debug!("Call kernel command {} with params {}", k_func, k_params);
                }
                0x54 | 0x55 => {
                    // self B
                    let selector = state.read_byte();
                    todo!("Call self {}", selector);
                }
                0x5c | 0x5d => {
                    // selfID
                    ax = current_obj.unwrap();
                }
                0x76 | 0x77 => {
                    // push0
                    stack.push(0);
                }
                0x78 | 0x79 => {
                    // push1
                    stack.push(1);
                }
                0x89 => {
                    // lsg B
                    let var = state.read_byte();
                    debug!("load global {} to stack", var);
                    stack.push(global_vars[var as usize]);
                }
                0xa1 => {
                    // sag B
                    // TODO: generalise this to all types 0x80..0xff
                    let var = state.read_byte();
                    debug!("store accumulator {} to global {}", ax, var);
                    global_vars[var as usize] = ax;
                }
                _ => {
                    todo!("Unknown command 0x{:x}", cmd);
                }
            }
            // TODO: do this for kWait
            // ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
        }
    }

    fn get_inherited_functions(
        &self,
        obj_class: &crate::script::ClassDefinition,
    ) -> HashMap<u16, (u16, u16)> {
        let mut func_selectors: HashMap<u16, (u16, u16)> = HashMap::new();

        for (s, offset) in &obj_class.function_selectors {
            func_selectors.insert(*s, (obj_class.script_number, *offset));
        }

        if obj_class.super_class != NO_SUPER_CLASS {
            let script_num = self.class_scripts[&obj_class.super_class];

            let script = self.load_script(script_num);

            let super_class_def = script.get_class(obj_class.super_class);
            func_selectors.extend(self.get_inherited_functions(super_class_def));
        }
        func_selectors
    }

    fn load_script(&self, number: u16) -> &Script {
        if let Some(script) = self.script_cache.get(&number) {
            return script;
        }
        let script = Script::load(
            resource::get_resource(&self.resources, ResourceType::Script, number).unwrap(),
        );

        self.script_cache.insert(number, Box::new(script))
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
