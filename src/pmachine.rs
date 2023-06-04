use std::{collections::HashMap, ffi::CStr};

use elsa::{FrozenMap, FrozenVec};
use itertools::Itertools;
use log::{debug, info};

use crate::{
    resource::{self, Resource, ResourceType},
    script::Script,
};

pub(crate) struct PMachine<'a> {
    resources: &'a HashMap<u16, Resource>,
    class_scripts: HashMap<u16, u16>,
    play_selector: u16,
    script_cache: FrozenMap<u16, Box<Script<'a>>>,
    object_cache: FrozenMap<String, Box<ObjectInstance>>,
    class_cache: FrozenMap<u16, Box<ObjectInstance>>,
}

enum VariableType {
    Global,
    Local,
    Temporary,
    Paramter,
}

struct StackFrame<'a> {
    stackframe_start: usize,
    params_pos: usize,
    temp_pos: usize,
    num_params: u16,
    script_number: u16,
    ip: usize,
    obj: &'a ObjectInstance,
}

struct MachineState<'a> {
    ip: usize, // instruction pointer
    // TODO: should this just be current_obj data and ip offset modified
    code: &'a [u8], // currently executing script data
    current_obj: &'a ObjectInstance,
}
impl MachineState<'_> {
    fn read_u8(&mut self) -> u8 {
        let v = self.code[self.ip];
        self.ip += 1;
        v
    }

    fn read_i8(&mut self) -> i8 {
        let v = i8::from_le_bytes(self.code[self.ip..self.ip + 1].try_into().unwrap());
        self.ip += 1;
        v
    }

    fn read_i16(&mut self) -> i16 {
        let v = i16::from_le_bytes(self.code[self.ip..self.ip + 2].try_into().unwrap());
        self.ip += 2;
        v
    }

    fn jump(&mut self, pos: i16) {
        assert!(self.ip <= i16::MAX as usize);
        let mut ip = self.ip as i16;
        ip += pos;
        self.ip = ip as usize;
    }

    fn read_u16(&mut self) -> u16 {
        // Currently a convenience, don't think we genuinely need unsigned
        let v = self.read_i16();
        assert!(v >= 0);
        v as u16
    }
}

#[derive(Debug)]
struct ObjectInstance {
    name: String,
    species: u16,
    variables: Vec<u16>,
    var_selectors: Vec<u16>,
    func_selectors: HashMap<u16, (u16, u16)>,
}
impl ObjectInstance {
    fn get_func_selector(&self, selector: u16) -> (u16, u16) {
        *self.func_selectors.get(&selector).unwrap()
    }

    fn has_var_selector(&self, selector: u16) -> bool {
        self.var_selectors.iter().contains(&selector)
    }

    fn get_property_by_offset(&self, offset: u8) -> u16 {
        // TODO: rename to properties?
        self.variables[offset as usize / 2]
    }
}

#[derive(Copy, Clone, Debug)]
enum Register<'a> {
    Value(i16),
    Object(&'a ObjectInstance),
    String(&'a String),
    // TODO: include heap pointers?
    Undefined,
}
impl Register<'_> {
    fn to_i16(&self) -> i16 {
        match *self {
            Register::Value(v) => v,
            _ => panic!("Register was not a value"),
        }
    }

    fn to_obj(&self) -> &ObjectInstance {
        match self {
            Register::Object(v) => v,
            _ => panic!("Register was not an object"),
        }
    }

    fn to_u16(&self) -> u16 {
        let v = self.to_i16();
        assert!(v >= 0);
        v as u16
    }
}

// TODO: should this be an enum?
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
            object_cache: FrozenMap::new(),
            class_cache: FrozenMap::new(),
        }
    }

    fn load_game_object(&self) -> &ObjectInstance {
        // load the play method from the game object (at exports[0]) in script 000
        let init_script = self.load_script(SCRIPT_MAIN);

        self.initialise_object(init_script.get_main_object())
    }

    fn initialise_object(&self, obj: &crate::script::ClassDefinition) -> &ObjectInstance {
        // TODO: we are going to need to deal with object's that get instantiated from a class or clone, so will not be uniquely identified by name in the cache. Put a handle in the register for these.
        todo!("return from cache or determine if this is creating a new one");
        let instance = ObjectInstance {
            name: String::from(&obj.name),
            species: obj.species,
            variables: obj.variables.clone(), // TODO: is clone necessary?
            var_selectors: obj.variable_selectors.clone(), // TODO: is clone necessary?
            func_selectors: self.get_inherited_functions(&obj),
        };
        self.object_cache
            .insert(String::from(&obj.name), Box::new(instance))
    }

    fn initialise_class(&self, class: &crate::script::ClassDefinition) -> &ObjectInstance {
        todo!("return from cache");
        todo!("Need to work out classes vs objects here");
        // TODO: should it be an object instance?
        let instance = ObjectInstance {
            name: String::from(&class.name),
            species: class.species,
            variables: class.variables.clone(), // TODO: is clone necessary?
            var_selectors: class.variable_selectors.clone(), // TODO: is clone necessary?
            func_selectors: self.get_inherited_functions(&class),
        };
        self.class_cache.insert(class.species, Box::new(instance))
    }

    pub(crate) fn run_game_play_method(&self) {
        let game_object = self.load_game_object();

        let (script_number, code_offset) = game_object.get_func_selector(self.play_selector);

        debug!(
            "Found play {} with code offset {:x?} in script {}",
            self.play_selector, code_offset, script_number
        );

        // This becomes the main loop
        // TODO: pass in event handling to be able to handle events and quit

        // TODO: better to do this by execution a machine function? Not happy with passing all the info in

        self.run(game_object, script_number, code_offset);
    }

    // TODO: consistent debug logging through here
    // TODO: log symbols so we can more easily debug it = opcodes, variables, selectors, classes etc.
    fn run(&self, run_obj: &ObjectInstance, run_script_number: u16, run_code_offset: u16) {
        // TODO: should not be reloading this again
        let mut script = self.load_script(run_script_number);

        let mut state = MachineState {
            code: script.data,
            ip: run_code_offset as usize,
            current_obj: run_obj,
        };

        let mut ax = Register::Undefined;

        let mut stack: Vec<Register> = Vec::new();

        let mut call_stack: Vec<StackFrame> = Vec::new();
        todo!("Sort out variable pointers approach");
        //var: [u16; 4], // variable points for each type (global, local, temporary, param) -- TODO: expand below instead?
        // TODO: maybe actually create slice to refer to?
        // TODO: try and clean these up and remove here as we use the call stack for multiple function selectors
        let mut num_params = 0; // TODO: is this relevant without a call stack?
        let mut params_pos = 0; // TODO: is this relevant without a call stack?
        let mut temp_pos = 0; // TODO: is this relevant without a call stack?

        // TODO: get variables from all loaded scripts, rather than loading again
        let mut global_vars: Vec<Register> = self
            .load_script(SCRIPT_MAIN)
            .variables
            .iter()
            .map(|&v| {
                todo!("Special casing a bit weird here, just treat as signed if we think safe");
                if v == 0xffff {
                    Register::Value(-1)
                } else {
                    assert!(v <= i16::MAX as u16);
                    Register::Value(v as i16)
                }
            })
            .collect_vec();

        loop {
            // TODO: break this out into a method and ensure there are good unit tests for the behaviours (e.g. the issues with num_params being wrong for call methods)
            let cmd = state.read_u8();
            debug!("[{}@{:x}] Executing {:x}", script.number, state.ip - 1, cmd);
            // TODO: do we do constants for opcodes? Do we enumberate the B / W variants or add tooling for this?
            //todo!("we need to check all the var indexes as they may be byte offsets not numbers in 0x80..0xff");
            match cmd {
                0x04 | 0x05 => {
                    // sub
                    // TODO: can we simplify all the unwrapping
                    ax = Register::Value(stack.pop().unwrap().to_i16() - ax.to_i16());
                }
                0x0c | 0x0d => {
                    // shr
                    ax = Register::Value(stack.pop().unwrap().to_i16() >> ax.to_i16());
                }
                0x12 | 0x13 => {
                    // and
                    ax = Register::Value(stack.pop().unwrap().to_i16() & ax.to_i16());
                }
                0x18 | 0x19 => {
                    // not
                    ax = Register::Value(if ax.to_i16() == 0 { 1 } else { 0 });
                }
                0x1a | 0x1b => {
                    // eq?
                    ax = Register::Value(if ax.to_i16() == stack.pop().unwrap().to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x1c | 0x1d => {
                    // ne?
                    ax = Register::Value(if ax.to_i16() != stack.pop().unwrap().to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x1e | 0x1f => {
                    // gt?
                    ax = Register::Value(if stack.pop().unwrap().to_i16() > ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x22 | 0x23 => {
                    // lt?
                    ax = Register::Value(if stack.pop().unwrap().to_i16() < ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x24 | 0x25 => {
                    // le?
                    ax = Register::Value(if stack.pop().unwrap().to_i16() <= ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x2e => {
                    // bt W
                    let pos = state.read_i16();
                    if ax.to_i16() != 0 {
                        state.jump(pos);
                    }
                }
                0x30 => {
                    // bnt W
                    let pos = state.read_i16();
                    if ax.to_i16() == 0 {
                        state.jump(pos);
                    }
                }
                0x32 => {
                    // jmp W
                    let pos = state.read_i16();
                    state.jump(pos);
                }
                0x34 => {
                    // ldi W
                    ax = Register::Value(state.read_i16());
                }
                0x35 => {
                    // ldi B
                    ax = Register::Value(state.read_i8() as i16);
                }
                0x36 => {
                    // push
                    stack.push(ax);
                }
                0x38 => {
                    // pushi W
                    stack.push(Register::Value(state.read_i16()));
                }
                0x39 => {
                    // pushi B
                    stack.push(Register::Value(state.read_i8() as i16));
                }
                0x3a | 0x3b => {
                    // toss
                    stack.pop();
                }
                0x3c | 0x3d => {
                    // dup
                    stack.push(*stack.last().unwrap());
                }
                0x3f => {
                    // link B
                    let num_variables = state.read_u8();
                    for _ in 0..num_variables {
                        stack.push(Register::Undefined);
                    }
                }
                0x40 => {
                    // call W relpos, B framesize
                    let rel_pos = state.read_i16();

                    todo!("can we reuse from send?");
                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2 - 1;

                    call_stack.push(StackFrame {
                        // Unwind position
                        stackframe_start,
                        // Saving these to return to
                        params_pos,
                        temp_pos,
                        num_params,
                        script_number: script.number,
                        ip: state.ip,
                        obj: state.current_obj,
                    });

                    // As opposed to send, does not start with selector
                    num_params = stack[stackframe_start].to_u16();
                    todo!("maybe replace < num_params with being < temp_pos below, though still an opportunity to make a better frame with params/temp separate");

                    state.jump(rel_pos);
                    params_pos = stackframe_start; // argc is included
                    temp_pos = stackframe_end;
                }
                0x43 => {
                    // callk B
                    let k_func = state.read_u8();
                    let k_params = state.read_u8() / 2;
                    let stackframe_start = stack.len() - (k_params as usize + 1);
                    let num_params = stack[stackframe_start].to_i16();
                    assert_eq!(num_params, k_params as i16);

                    // call command, put return value into ax
                    todo!("separate method probably");
                    todo!("Set up a parameter slice rather than indexing into the stack");
                    match k_func {
                        0x00 => {
                            // Load
                            let res_type = stack[stackframe_start + 1].to_i16() & 0x7F;
                            let res_num = stack[stackframe_start + 2].to_i16();
                            info!("Kernel> Load res_type: {}, res_num: {}", res_type, res_num);
                            // TODO: load it and put a "pointer" into ax -- how is it used?
                        }
                        0x02 => {
                            // ScriptID
                            let script_number = stack[stackframe_start + 1].to_i16();
                            let dispatch_number = if num_params > 1 {
                                stack[stackframe_start + 2].to_i16()
                            } else {
                                0
                            };
                            info!(
                                "Kernel> ScriptID script_number: {}, dispatch_number: {}",
                                script_number, dispatch_number
                            );
                            // TODO: load it and put a "pointer" into ax
                            // todo!("This is not correct, temporary");
                            ax = Register::Object(state.current_obj);
                        }
                        0x04 => {
                            // Clone
                            let obj = stack[stackframe_start + 1].to_obj();
                            info!("Kernel> Clone obj: {}", obj.name);
                            // TODO: clone it, update info and selectors as documented
                            // TODO: put the heap ptr into ax
                        }
                        0x0b => {
                            // Animate
                            info!("Kernel> Animate");
                            // TODO: get all the params, animate. No return value
                        }
                        0x1c => {
                            // GetEvent
                            let flags = stack[stackframe_start + 1].to_i16();
                            let event = stack[stackframe_start + 2].to_obj(); // TODO: how do we convert this into an object instance that we can mutate?
                            info!("Kernel> GetEvent flags: {:x}, event: {}", flags, event.name);
                            // TODO: check the events, but for now just return null event
                            ax = Register::Value(0);
                        }
                        0x45 => {
                            // Wait
                            let ticks = stack[stackframe_start + 1].to_i16();
                            // TODO: do wait, set return value
                            info!("Kernel> Wait ticks: {:x}", ticks);
                            // todo!("Temporary - currently just setting this to quit so it doesn't infinite loop");
                            global_vars[4] = Register::Value(1);
                            // TODO: do this for kWait
                            // ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
                        }
                        _ => {
                            debug!("Call kernel command {:x} with #params {}", k_func, k_params);
                            // todo!("Implement missing kernel command");
                            // TODO: temp assuming it returns a value
                            ax = Register::Value(0);
                        }
                    }

                    // unwind stack
                    // TODO: do we need to do something with rest?
                    stack.truncate(stackframe_start);
                }
                0x48 | 0x49 => {
                    // ret
                    if call_stack.is_empty() {
                        return;
                    }

                    let frame = call_stack.pop().unwrap();
                    debug!(
                        "Return from function -> {}@{:x}",
                        frame.script_number, frame.ip
                    );
                    let current_script = script.number;
                    if frame.script_number != current_script {
                        script = self.load_script(frame.script_number);
                        state.code = script.data;
                    };
                    state.ip = frame.ip;
                    state.current_obj = frame.obj;

                    let unwind_pos = frame.stackframe_start;
                    stack.truncate(unwind_pos);
                    params_pos = frame.params_pos;
                    temp_pos = frame.temp_pos;
                    num_params = frame.num_params;
                }
                0x4a | 0x4b => {
                    // send B
                    todo!("This will need to change when objects are on the heap and not a single cache");
                    // TODO: bit awkward, to appease the borrow checker of what is stored in ax
                    let obj = if let Some(o) = self.object_cache.get(&ax.to_obj().name) {
                        o
                    } else if let Some(c) = self.class_cache.get(&ax.to_obj().species) {
                        c
                    } else {
                        todo!(
                            "Didn't find ax object {} in objects or classes",
                            &ax.to_obj().name
                        );
                    };

                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2;

                    let mut read_selectors_idx = stackframe_start;
                    let mut selectors = Vec::new();
                    while read_selectors_idx < stackframe_end {
                        // TODO: types
                        let (selector, np) = (
                            stack[read_selectors_idx].to_u16(),
                            stack[read_selectors_idx + 1].to_u16(),
                        );
                        read_selectors_idx += 1;
                        selectors.push((selector, np, read_selectors_idx));
                        read_selectors_idx += np as usize + 1;
                    }

                    // TODO: super temporary to get this working
                    let mut count = 0;
                    for (selector, np, pos) in &selectors {
                        count += 1;
                        if obj.has_var_selector(*selector) {
                            if *np == 0 {
                                // get
                                ax = Register::Value(0); // TODO: set to the variable value if no paramters
                                todo!();
                            } else {
                                // TODO: set
                                todo!("how to handle mutating object");
                            }
                            if count == selectors.len() {
                                // Unwind stack as ret will not be called
                                stack.truncate(stackframe_start);
                            }
                        } else {
                            todo!("assert last one, we don't have a way to recursively send for functions yet");
                            assert_eq!(count, selectors.len());

                            let (script_number, code_offset) = obj.get_func_selector(*selector);
                            debug!(
                                "Call send on function {selector} -> {script_number} @{:x} {}",
                                code_offset, obj.name
                            ); // TODO: show parameters?

                            let current_script = script.number;

                            call_stack.push(StackFrame {
                                // Unwind position
                                stackframe_start,
                                // Saving these to return to
                                params_pos,
                                temp_pos,
                                num_params,
                                script_number: current_script,
                                ip: state.ip,
                                obj: state.current_obj,
                            });

                            if script_number != current_script {
                                script = self.load_script(script_number);
                                state.code = script.data;
                            };
                            state.ip = code_offset as usize;
                            state.current_obj = obj;
                            params_pos = *pos;
                            temp_pos = stackframe_end;
                            num_params = *np;
                        }
                    }
                }
                0x51 => {
                    // class B
                    let num = state.read_u8() as u16;

                    let script_number = self.class_scripts[&num];
                    let s = if script_number != script.number {
                        self.load_script(script_number)
                    } else {
                        script
                    };

                    let class = s.get_class(num);
                    debug!("class {} {}", class.script_number, num);
                    let instance = self.initialise_class(class);
                    ax = Register::Object(instance);
                }
                0x54 | 0x55 => {
                    // self B selector
                    let obj = state.current_obj;

                    todo!("reuse copy-pasta from send");
                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2;
                    let stackframe = &stack[stackframe_start..];
                    let selector = stackframe[0].to_u16();
                    let np = stackframe[1].to_u16();
                    // TODO: this will fail if multiple selectors
                    assert_eq!(stackframe_end, stackframe_start + 2 + np as usize);

                    // TODO: handle variables as well
                    let (script_number, code_offset) = obj.get_func_selector(selector);
                    debug!("Call self {selector} -> {script_number} @{:x}", code_offset); // TODO: show parameters?

                    let current_script = script.number;

                    call_stack.push(StackFrame {
                        // Unwind position
                        stackframe_start,
                        // Saved to return to
                        params_pos,
                        temp_pos,
                        num_params,
                        script_number: current_script,
                        ip: state.ip,
                        obj: state.current_obj,
                    });

                    if script_number != current_script {
                        script = self.load_script(script_number);
                        state.code = script.data;
                    };
                    state.ip = code_offset as usize;
                    // TODO: do we need to change current_obj in self? What about if it is in superClass?
                    params_pos = stackframe_start + 1; // skip the selector
                    temp_pos = stackframe_end;
                    num_params = np;
                }
                0x57 => {
                    // super B class B stackframe
                    let class_num = state.read_u8() as u16;

                    todo!("re-use copy-pasta from class");
                    let script_number = self.class_scripts[&class_num];
                    let s = if script_number != script.number {
                        self.load_script(script_number)
                    } else {
                        script
                    };

                    let class = s.get_class(class_num);
                    let obj = self.initialise_class(class);

                    todo!("re-use copy-pasta from send");
                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2;
                    let stackframe = &stack[stackframe_start..];
                    let selector = stackframe[0].to_u16();
                    let np = stackframe[1].to_u16();
                    // TODO: this will fail if multiple selectors
                    assert_eq!(stackframe_end, stackframe_start + 2 + np as usize);

                    // Note there's an assumption that variables are not sent for here since it is a class not an object
                    let (script_number, code_offset) = obj.get_func_selector(selector);
                    debug!(
                        "Call super {class_num} {selector} -> {script_number} @{:x}",
                        code_offset
                    ); // TODO: show parameters?

                    let current_script = script.number;

                    call_stack.push(StackFrame {
                        // Unwind position
                        stackframe_start,
                        // Save these to return to
                        params_pos,
                        temp_pos,
                        num_params,
                        script_number: current_script,
                        ip: state.ip,
                        obj: state.current_obj,
                    });

                    if script_number != current_script {
                        script = self.load_script(script_number);
                        state.code = script.data;
                    };
                    state.ip = code_offset as usize;
                    state.current_obj = obj;
                    params_pos = stackframe_start + 1; // skip the selector
                    temp_pos = stackframe_end;
                    num_params = np;
                }
                0x5b => {
                    // lea B type, B index
                    let var_type = state.read_i8();
                    let var_index = state.read_u8();

                    let use_acc = (var_type & 0b10000) != 0;
                    let var_type_num = var_type & 0b110 >> 1; // TODO: convert to VariableType and then match

                    todo!("Load effective address {var_type_num} {use_acc} {var_index}");
                    // TODO: ax = &(vars[var_type_num][use_acc ? vi+acc : vi])
                }
                0x5c | 0x5d => {
                    // selfID
                    ax = Register::Object(state.current_obj);
                }
                0x63 => {
                    // pToa B offset
                    let offset = state.read_u8();
                    debug!("property @offset {offset} to acc");
                    ax = Register::Value(state.current_obj.get_property_by_offset(offset) as i16);
                    todo!("assert we didn't get a large unsigned value");
                    todo!("can a variable be an object though?");
                }
                0x65 => {
                    // aTop B offset
                    let offset = state.read_u8();
                    debug!("acc to property @offset {offset}");
                    todo!("How to handle mutating current_obj?");
                    // state
                    //     .current_obj
                    //     .set_property_by_offset(offset, ax.to_i16());
                    // self.variables[offset as usize / 2] = value as u16;
                    todo!("assert we didn't get a large unsigned value");
                    todo!("can a variable be an object though?");
                }
                0x67 => {
                    // pTos B offset
                    let offset = state.read_u8();
                    debug!("property @offset {offset} to stack");
                    stack.push(Register::Value(
                        state.current_obj.get_property_by_offset(offset) as i16,
                    ));
                    todo!("assert we didn't get a large unsigned value");
                    todo!("can a variable be an object though?");
                }
                0x6b => {
                    // ipToa B offset
                    let offset = state.read_u8();
                    debug!("increment property @offset {offset} to acc");
                    todo!("increment the property - how to handle mutation");
                    // This will probably infinite loop
                    ax = Register::Value(state.current_obj.get_property_by_offset(offset) as i16);
                    todo!("assert we didn't get a large unsigned value");
                    todo!("can a variable be an object though?");
                }
                0x72 => {
                    // lofsa W
                    let offset = state.read_i16();
                    debug!("Load offset {} to acc", offset);
                    assert!(state.ip <= i16::MAX as usize); // Make sure this isn't a bad cast

                    // Need to check what it is at this address
                    // TODO: can we generalise what the script gives back by a type?
                    let v = (state.ip as i16 + offset) as usize;
                    ax = if let Some(obj) = script.get_object_by_offset(v) {
                        Register::Object(self.initialise_object(obj))
                    } else if let Some(s) = script.get_string_by_offset(v) {
                        Register::String(&s.string)
                    } else {
                        // TODO: may need to put a whole lot of handles into script?
                        // TODO: support 'said'
                        todo!("Unknown method loading from address {:x}", v);
                        Register::Undefined
                    };
                }
                0x76 | 0x77 => {
                    // push0
                    stack.push(Register::Value(0));
                }
                0x78 | 0x79 => {
                    // push1
                    stack.push(Register::Value(1));
                }
                0x7a | 0x7b => {
                    // push2
                    stack.push(Register::Value(2));
                }
                0x7c => {
                    // pushSelf
                    stack.push(Register::Object(state.current_obj));
                }
                // TODO: generalise this to all types 0x80..0xff
                0x81 => {
                    // lag B
                    let var = state.read_u8() as usize;
                    debug!("load global {} to acc", var);
                    ax = global_vars[var];
                }
                0x85 => {
                    // lat B
                    let var = state.read_u8();
                    debug!("load temp {} to acc", var);
                    ax = stack[temp_pos + var as usize];
                }
                0x87 => {
                    // lap B
                    let var = state.read_u8() as u16;
                    debug!("load parameter {} to acc", var);

                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    assert!(var <= num_params);
                    ax = stack[params_pos + var as usize];
                }
                0x89 => {
                    // lsg B
                    let var = state.read_u8() as usize;
                    debug!("load global {} to stack", var);
                    stack.push(global_vars[var]);
                }
                0x8d => {
                    // lst B
                    let var = state.read_u8();
                    debug!("load temp {} to stack", var);
                    stack.push(stack[temp_pos + var as usize]);
                }
                0x8f => {
                    // lsp B
                    let var = state.read_u8() as u16;
                    debug!("load parameter {} to stack", var);
                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    assert!(var <= num_params);
                    stack.push(stack[params_pos + var as usize]);
                }
                0x97 => {
                    // lapi B
                    let var = state.read_u8() as u16 + ax.to_u16();
                    debug!("load parameter {} to acc", var);
                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    assert!(var <= num_params);
                    ax = stack[params_pos + var as usize];
                }
                0x98 => {
                    // lsgi W
                    let var = state.read_u16() + ax.to_u16();
                    debug!("load global {} to stack", var);
                    stack.push(global_vars[var as usize]);
                }
                0xa0 => {
                    // sag W
                    let var = state.read_u16();
                    debug!("store accumulator to global {}", var);
                    global_vars[var as usize] = ax;
                }
                0xa1 => {
                    // sag B
                    let var = state.read_u8();
                    debug!("store accumulator to global {}", var);
                    global_vars[var as usize] = ax;
                }
                0xa3 => {
                    // sal B
                    let var = state.read_u8();
                    debug!("store accumulator to local {}", var);
                    todo!("store local var");
                }
                0xa5 => {
                    // sat B
                    let var = state.read_u8() as usize;
                    debug!("store accumulator to temp {}", var);
                    stack[temp_pos + var] = ax;
                }
                0xa7 => {
                    // sap B
                    let var = state.read_u8() as usize;
                    debug!("store accumulator to param {}", var);
                    stack[params_pos + var] = ax;
                }
                0xb0 => {
                    // sagi W
                    let var = state.read_u16();
                    let idx = var + ax.to_u16();
                    debug!("store accumulator {} to global {}", ax.to_u16(), idx);
                    global_vars[idx as usize] = ax;
                }
                0xc5 => {
                    // +at B
                    let var = state.read_u8() as usize;
                    let v = stack[temp_pos + var].to_i16() + 1;
                    stack[temp_pos + var] = Register::Value(v);
                    ax = stack[temp_pos + var];
                }
                _ => {
                    todo!("Unknown command 0x{:x}", cmd);
                }
            }
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
            for (k, v) in self.get_inherited_functions(super_class_def) {
                func_selectors.entry(k).or_insert(v);
            }
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
