use std::{borrow::Borrow, cell::RefCell, collections::HashMap};

use elsa::FrozenMap;
use itertools::Itertools;
use log::{debug, info};
use num_traits::FromPrimitive;

use crate::{
    resource::{self, Resource, ResourceType},
    script::Script,
};

pub(crate) struct PMachine<'a> {
    resources: &'a HashMap<u16, Resource>,
    class_scripts: HashMap<u16, u16>,
    play_selector: u16,
    script_cache: FrozenMap<u16, Box<Script>>,
    object_cache: FrozenMap<usize, Box<ObjectInstance>>,
}

#[derive(FromPrimitive, Copy, Clone, Debug)]
enum VariableType {
    Global,
    Local,
    Temporary,
    Paramter,
}

enum ClassInfo {
    Object,
    Clone,
    Class = 0x8000,
}

struct StackFrame {
    stackframe_start: usize,
    params_pos: usize,
    temp_pos: usize,
    num_params: u16,
    script_number: u16,
    ip: usize,
    obj: usize,
    remaining_selectors: Vec<usize>,
}

struct MachineState<'a> {
    ip: usize, // instruction pointer
    // TODO: should this just be current_obj data and ip offset modified
    code: Box<Vec<u8>>, // currently executing script data
    current_obj: &'a ObjectInstance,
    ax: Register,

    // Stack information
    // TODO: better approach than this to create a frame?
    params_pos: usize,
    temp_pos: usize,
    num_params: u16,
    script: u16,
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
    id: usize,
    name: String,
    species: u16,
    variables: RefCell<Vec<Register>>,
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

    fn get_property_by_offset(&self, offset: u8) -> Register {
        // TODO: rename to properties?
        self.variables.borrow()[offset as usize / 2]
    }

    fn set_property_by_offset(&self, offset: u8, value: Register) {
        self.variables.borrow_mut()[offset as usize / 2] = value
    }

    fn get_property(&self, selector: u16) -> Register {
        let (idx, _) = self
            .var_selectors
            .iter()
            .find_position(|&s| *s == selector)
            .unwrap();
        self.variables.borrow()[idx]
    }

    fn set_property(&self, selector: u16, value: Register) {
        let (idx, _) = self
            .var_selectors
            .iter()
            .find_position(|&s| *s == selector)
            .unwrap();
        self.variables.borrow_mut()[idx] = value
    }
}

// TODO: get rid of lifetime in here
#[derive(Copy, Clone, Debug)]
enum Register {
    Value(i16),
    Object(usize),
    Variable(VariableType, i16),
    String(usize),
    // TODO: include heap pointers?
    Undefined,
}
impl Register {
    fn to_i16(&self) -> i16 {
        match *self {
            Register::Value(v) => v,
            _ => panic!("Register was not a value"),
        }
    }

    fn to_obj(&self) -> usize {
        match *self {
            Register::Object(v) => v,
            _ => panic!("Register was not an object"),
        }
    }

    fn to_u16(&self) -> u16 {
        let v = self.to_i16();
        assert!(v >= 0);
        v as u16
    }

    fn is_zero_or_null(&self) -> bool {
        match *self {
            Register::Value(v) => v == 0,
            Register::Object(v) => v == 0,
            _ => panic!("Register {:?} doesn't have a zero value", *self),
        }
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
        }
    }

    fn load_game_object(&self) -> &ObjectInstance {
        // load the play method from the game object (at exports[0]) in script 000
        let init_script = self.load_script(SCRIPT_MAIN);

        self.initialise_object(init_script.get_main_object())
    }

    fn initialise_object(&self, obj: &crate::script::ClassDefinition) -> &ObjectInstance {
        if let Some(o) = self.object_cache.get(&obj.id()) {
            return o;
        }
        // TODO: we are going to need to deal with object's that get instantiated from a class or clone,
        // in those cases we need to adjust the key from script+offset, perhaps clone can be (script+1000,ref_count)

        let var_selectors = if obj.info == ClassInfo::Class as u16 {
            // Class
            obj.variable_selectors.clone()
        } else {
            // Object
            self.get_inherited_var_selectors(&obj).clone()
        };

        let instance = ObjectInstance {
            id: obj.id(),
            name: String::from(&obj.name),
            species: obj.species,
            variables: RefCell::new(
                obj.variables
                    .iter()
                    .map(|&v| Register::Value(v as i16)) // TODO: check cast
                    .collect_vec(),
            ), // TODO: is clone necessary?
            var_selectors,
            func_selectors: self.get_inherited_functions(&obj),
        };
        self.object_cache.insert(obj.id(), Box::new(instance))
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
        let s = self.load_script(run_script_number);

        // TODO: separate registers and stack frames, can keep some of this out of the send to selector function which is just creating a call
        let mut state = MachineState {
            code: s.data.clone(), // TODO: remove clone
            ip: run_code_offset as usize,
            current_obj: run_obj,
            ax: Register::Undefined,
            params_pos: 0, // TODO: is this relevant without a call stack?
            num_params: 0, // TODO: is this relevant without a call stack?
            temp_pos: 0,   // TODO: is this relevant without a call stack?
            script: run_script_number,
        };

        let mut stack: Vec<Register> = Vec::new();

        let mut call_stack: Vec<StackFrame> = Vec::new();

        // TODO: get variables from all loaded scripts, rather than loading again
        let mut global_vars: Vec<Register> = self
            .load_script(SCRIPT_MAIN)
            .variables
            .borrow()
            .iter()
            .map(|&v| {
                // TODO: remove this assertion when we are confident an i16 can be used
                assert!(v <= i16::MAX as u16 || v == 0xffff);
                Register::Value(v as i16)
            })
            .collect_vec();

        loop {
            // TODO: break this out into a method and ensure there are good unit tests for the behaviours (e.g. the issues with num_params being wrong for call methods)
            let cmd = state.read_u8();
            debug!("[{}@{:x}] Executing {:x}", state.script, state.ip - 1, cmd);
            // TODO: do we do constants for opcodes? Do we enumberate the B / W variants or add tooling for this?
            //todo!("we need to check all the var indexes as they may be byte offsets not numbers in 0x80..0xff");
            match cmd {
                0x04 | 0x05 => {
                    // sub
                    // TODO: can we simplify all the unwrapping
                    state.ax = Register::Value(stack.pop().unwrap().to_i16() - state.ax.to_i16());
                }
                0x0c | 0x0d => {
                    // shr
                    state.ax = Register::Value(stack.pop().unwrap().to_i16() >> state.ax.to_i16());
                }
                0x12 | 0x13 => {
                    // and
                    state.ax = Register::Value(stack.pop().unwrap().to_i16() & state.ax.to_i16());
                }
                0x14 | 0x15 => {
                    // or
                    state.ax = Register::Value(stack.pop().unwrap().to_i16() | state.ax.to_i16());
                }
                0x18 | 0x19 => {
                    // not
                    state.ax = Register::Value(if state.ax.to_i16() == 0 { 1 } else { 0 });
                }
                0x1a | 0x1b => {
                    // eq?
                    state.ax =
                        Register::Value(if state.ax.to_i16() == stack.pop().unwrap().to_i16() {
                            1
                        } else {
                            0
                        });
                }
                0x1c | 0x1d => {
                    // ne?
                    state.ax =
                        Register::Value(if state.ax.to_i16() != stack.pop().unwrap().to_i16() {
                            1
                        } else {
                            0
                        });
                }
                0x1e | 0x1f => {
                    // gt?
                    state.ax =
                        Register::Value(if stack.pop().unwrap().to_i16() > state.ax.to_i16() {
                            1
                        } else {
                            0
                        });
                }
                0x22 | 0x23 => {
                    // lt?
                    state.ax =
                        Register::Value(if stack.pop().unwrap().to_i16() < state.ax.to_i16() {
                            1
                        } else {
                            0
                        });
                }
                0x24 | 0x25 => {
                    // le?
                    state.ax =
                        Register::Value(if stack.pop().unwrap().to_i16() <= state.ax.to_i16() {
                            1
                        } else {
                            0
                        });
                }
                0x2e => {
                    // bt W
                    let pos = state.read_i16();
                    if state.ax.to_i16() != 0 {
                        state.jump(pos);
                    }
                }
                0x30 => {
                    // bnt W
                    let pos = state.read_i16();
                    if state.ax.is_zero_or_null() {
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
                    state.ax = Register::Value(state.read_i16());
                }
                0x35 => {
                    // ldi B
                    state.ax = Register::Value(state.read_i8() as i16);
                }
                0x36 => {
                    // push
                    stack.push(state.ax);
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

                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2 - 1;

                    call_stack.push(StackFrame {
                        // Unwind position
                        stackframe_start,
                        // Saving these to return to
                        params_pos: state.params_pos,
                        temp_pos: state.temp_pos,
                        num_params: state.num_params,
                        script_number: state.script,
                        ip: state.ip,
                        obj: state.current_obj.id,
                        remaining_selectors: Vec::new(),
                    });

                    // As opposed to send, does not start with selector
                    state.num_params = stack[stackframe_start].to_u16();

                    state.jump(rel_pos);
                    state.params_pos = stackframe_start; // argc is included
                    state.temp_pos = stackframe_end;
                }
                0x43 => {
                    // callk B
                    let k_func = state.read_u8();
                    let k_params = state.read_u8() as usize / 2;
                    let stackframe_start = stack.len() - (k_params + 1);
                    let params = &stack[stackframe_start..];

                    let num_params = params[0].to_i16();
                    assert_eq!(num_params, k_params as i16);

                    // call command, put return value into ax
                    if let Some(value) = self.call_kernel_command(k_func, params) {
                        state.ax = value;
                    }

                    // todo!("Temporary - currently just setting this to quit so it doesn't infinite loop");
                    if k_func == 0x45 {
                        global_vars[4] = Register::Value(1);
                    }

                    // unwind stack
                    // TODO: do we need to do something with rest?
                    stack.truncate(stackframe_start);
                }
                0x45 => {
                    // callb B dispindex, B framesize
                    let dispatch_index = state.read_u8() as i16;

                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2 - 1;

                    call_stack.push(StackFrame {
                        // Unwind position
                        stackframe_start,
                        // Saving these to return to
                        params_pos: state.params_pos,
                        temp_pos: state.temp_pos,
                        num_params: state.num_params,
                        script_number: state.script,
                        ip: state.ip,
                        obj: state.current_obj.id,
                        remaining_selectors: Vec::new(),
                    });

                    // As opposed to send, does not start with selector
                    state.num_params = stack[stackframe_start].to_u16();

                    // Switch to base script
                    // todo!("this combination should be done in state to ensure it's always done together");
                    state.script = SCRIPT_MAIN;
                    let script = self.load_script(state.script);
                    state.code = script.data.clone(); // TODO: remove clone
                    state.ip = script.get_dispatch_address(dispatch_index) as usize;

                    state.params_pos = stackframe_start; // argc is included
                    state.temp_pos = stackframe_end;
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

                    let previous_obj = state.current_obj;
                    let script = self.load_script(frame.script_number);
                    state.code = script.data.clone(); // TODO: remove clone
                    state.ip = frame.ip;
                    state.current_obj = self.object_cache.get(&frame.obj).unwrap();

                    state.params_pos = frame.params_pos;
                    state.temp_pos = frame.temp_pos;
                    state.num_params = frame.num_params;
                    state.script = frame.script_number;

                    let remaining_selectors = &mut frame.remaining_selectors.clone(); // TODO: is clone needed?
                    while !remaining_selectors.is_empty() {
                        let obj = previous_obj;
                        let start = frame.stackframe_start + remaining_selectors.pop().unwrap();
                        let selector = stack[start].to_u16();
                        let pos = start + 1;
                        let np = stack[pos].to_u16();
                        if let Some(frame) = self.send_to_selector(
                            &mut state,
                            &stack[pos..=pos + np as usize],
                            obj,
                            selector,
                            np,
                            frame.stackframe_start,
                            frame.temp_pos,
                            pos,
                            remaining_selectors.clone(), // TODO: is clone needed
                        ) {
                            state.current_obj = obj;
                            call_stack.push(frame);
                            break;
                        }
                        if remaining_selectors.is_empty() {
                            // Unwind stack as ret will not be called
                            let unwind_pos = frame.stackframe_start;
                            stack.truncate(unwind_pos);
                        }
                    }
                }
                0x4a | 0x4b | 0x54 | 0x55 | 0x57 => {
                    // TODO: factor out a method and make this separate again. Curently hard with local variables in here.
                    let obj = if cmd == 0x54 || cmd == 0x55 {
                        // self B selector
                        state.current_obj
                    } else if cmd == 0x57 {
                        // super B class B stackframe
                        let class_num = state.read_u8() as u16;
                        self.initialise_object_from_class(class_num)
                    } else {
                        // send B
                        self.object_cache.get(&state.ax.to_obj()).unwrap()
                    };

                    // TODO: instead of just pushing onto an execution stack and looping
                    // it would be good to start a new context like run so we can just pop the whole thing on return

                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start = stackframe_end - stackframe_size / 2;

                    let mut read_selectors_idx = 0;
                    let mut selector_offsets = Vec::new();
                    while read_selectors_idx < stackframe_end - stackframe_start {
                        let np = stack[stackframe_start + read_selectors_idx + 1].to_u16();
                        selector_offsets.push(read_selectors_idx);
                        read_selectors_idx += np as usize + 2;
                    }

                    debug!(
                        "Sending to {} selectors for {}",
                        selector_offsets.len(),
                        obj.name
                    );
                    while !selector_offsets.is_empty() {
                        let start = stackframe_start + selector_offsets.pop().unwrap();
                        let selector = stack[start].to_u16();
                        let pos = start + 1;
                        let np = stack[pos].to_u16();
                        if let Some(frame) = self.send_to_selector(
                            &mut state,
                            &stack[pos..=pos + np as usize],
                            obj,
                            selector,
                            np,
                            stackframe_start,
                            stackframe_end,
                            pos,
                            selector_offsets.clone(), // TODO: is clone needed?
                        ) {
                            state.current_obj = obj;
                            call_stack.push(frame);
                            break;
                        }
                        if selector_offsets.is_empty() {
                            // Unwind stack as ret will not be called
                            stack.truncate(stackframe_start);
                        }
                    }
                }
                0x51 => {
                    // class B
                    let class_num = state.read_u8() as u16;
                    let obj = self.initialise_object_from_class(class_num);

                    // TODO: do we need to change script?
                    state.ax = Register::Object(obj.id);
                }
                0x5b => {
                    // lea B type, B index
                    let var_type = state.read_i8();
                    let mut var_index = state.read_u8() as i16;

                    // TODO: use bitflags
                    let use_acc = (var_type & 0b10000) != 0;
                    let var_type_num = var_type & 0b110 >> 1;

                    if use_acc {
                        var_index += state.ax.to_i16();
                        assert!(var_index >= 0);
                    }

                    let variable_type: VariableType =
                        FromPrimitive::from_u16(var_type_num as u16).unwrap();

                    // TODO: confirm that this is correct - get the variable "address", not the value
                    state.ax = Register::Variable(variable_type, var_index);
                }
                0x5c | 0x5d => {
                    // selfID
                    state.ax = Register::Object(state.current_obj.id);
                }
                0x63 => {
                    // pToa B offset
                    let offset = state.read_u8();
                    debug!("property @offset {offset} to acc");
                    state.ax = state.current_obj.get_property_by_offset(offset);
                }
                0x65 => {
                    // aTop B offset
                    let offset = state.read_u8();
                    debug!("acc to property @offset {offset}");
                    state.current_obj.set_property_by_offset(offset, state.ax);
                }
                0x67 => {
                    // pTos B offset
                    let offset = state.read_u8();
                    debug!("property @offset {offset} to stack");
                    stack.push(state.current_obj.get_property_by_offset(offset));
                }
                0x6b => {
                    // ipToa B offset
                    let offset = state.read_u8();
                    debug!("increment property @offset {offset} to acc");
                    state.ax = state.current_obj.get_property_by_offset(offset);
                    state
                        .current_obj
                        .set_property_by_offset(offset, Register::Value(state.ax.to_i16() + 1));
                }
                0x72 => {
                    // lofsa W
                    let offset = state.read_i16();
                    debug!("Load offset {} to acc", offset);
                    assert!(state.ip <= i16::MAX as usize); // Make sure this isn't a bad cast

                    // Need to check what it is at this address
                    // TODO: can we generalise what the script gives back by a type?
                    let v = (state.ip as i16 + offset) as usize;
                    let script = self.load_script(state.script);
                    state.ax = if let Some(obj) = script.get_object_by_offset(v) {
                        Register::Object(self.initialise_object(obj).id)
                    } else if let Some(s) = script.get_string_by_offset(v) {
                        Register::String(s.offset)
                    } else {
                        // TODO: may need to put a whole lot of handles into script?
                        // TODO: support 'said'
                        // todo!("Unknown method loading from address {:x}", v);
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
                    stack.push(Register::Object(state.current_obj.id));
                }
                // TODO: generalise this to all types 0x80..0xff
                0x81 => {
                    // lag B
                    let var = state.read_u8() as usize;
                    debug!("load global {} to acc", var);
                    state.ax = global_vars[var];
                }
                0x85 => {
                    // lat B
                    let var = state.read_u8();
                    debug!("load temp {} to acc", var);
                    state.ax = stack[state.temp_pos + var as usize];
                }
                0x87 => {
                    // lap B
                    let var = state.read_u8() as u16;
                    debug!("load parameter {} to acc", var);

                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    if var <= state.num_params {
                        state.ax = stack[state.params_pos + var as usize];
                    }
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
                    stack.push(stack[state.temp_pos + var as usize]);
                }
                0x8f => {
                    // lsp B
                    let var = state.read_u8() as u16;
                    debug!("load parameter {} to stack", var);
                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    if var <= state.num_params {
                        stack.push(stack[state.params_pos + var as usize]);
                    }
                }
                0x97 => {
                    // lapi B
                    let var = state.read_u8() as u16 + state.ax.to_u16();
                    debug!("load parameter {} to acc", var);
                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    if var <= state.num_params {
                        state.ax = stack[state.params_pos + var as usize];
                    }
                }
                0x98 => {
                    // lsgi W
                    let var = state.read_u16() + state.ax.to_u16();
                    debug!("load global {} to stack", var);
                    stack.push(global_vars[var as usize]);
                }
                0xa0 => {
                    // sag W
                    let var = state.read_u16();
                    debug!("store accumulator to global {}", var);
                    global_vars[var as usize] = state.ax;
                }
                0xa1 => {
                    // sag B
                    let var = state.read_u8();
                    debug!("store accumulator to global {}", var);
                    global_vars[var as usize] = state.ax;
                }
                0xa3 => {
                    // sal B
                    let var = state.read_u8();
                    debug!("store accumulator to local {}", var);
                    // TODO! I'm guessing this needs to be the current_obj, not this script
                    let script = self.load_script(state.script);
                    script.variables.borrow_mut()[var as usize] = state.ax.to_u16();
                }
                0xa5 => {
                    // sat B
                    let var = state.read_u8() as usize;
                    debug!("store accumulator to temp {}", var);
                    stack[state.temp_pos + var] = state.ax;
                }
                0xa7 => {
                    // sap B
                    let var = state.read_u8() as usize;
                    debug!("store accumulator to param {}", var);
                    stack[state.params_pos + var] = state.ax;
                }
                0xb0 => {
                    // sagi W
                    let var = state.read_u16();
                    let idx = var + state.ax.to_u16();
                    debug!("store accumulator {} to global {}", state.ax.to_u16(), idx);
                    global_vars[idx as usize] = state.ax;
                }
                0xc5 => {
                    // +at B
                    let var = state.read_u8() as usize;
                    let idx = state.temp_pos + var;
                    let v = stack[idx].to_i16() + 1;
                    stack[idx] = Register::Value(v);
                    state.ax = stack[idx];
                }
                _ => {
                    todo!("Unknown command 0x{:x}", cmd);
                }
            }
        }
    }

    fn get_inherited_var_selectors(&self, obj_class: &crate::script::ClassDefinition) -> &Vec<u16> {
        let script_num = self.class_scripts[&obj_class.super_class];
        let script = self.load_script(script_num);
        let super_class_def = script.get_class(obj_class.super_class);
        &super_class_def.variable_selectors
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

    fn initialise_object_from_class(&self, class_num: u16) -> &ObjectInstance {
        let script_number = self.class_scripts[&class_num];
        let script = self.load_script(script_number);
        let class = script.get_class(class_num);
        self.initialise_object(class)
    }

    fn send_to_selector(
        &self,
        state: &mut MachineState,
        params: &[Register],
        obj: &ObjectInstance,
        selector: u16,
        num_params: u16,
        // TODO: remove these params
        stackframe_start: usize,
        stackframe_end: usize,
        params_pos: usize,
        selector_offsets: Vec<usize>,
    ) -> Option<StackFrame> {
        debug!("Sending to selector {:x} for {}", selector, obj.name); // TODO: params

        if obj.has_var_selector(selector) {
            // Variable
            if num_params == 0 {
                // get
                state.ax = obj.get_property(selector);
            } else {
                obj.set_property(selector, params[1]);
            }
            None
        } else {
            // Function

            let (script_number, code_offset) = obj.get_func_selector(selector);
            debug!(
                "Call send on function {selector} -> {script_number} @{:x} for {} #p: {}",
                code_offset, obj.name, num_params
            );

            let frame = StackFrame {
                // Unwind position
                stackframe_start,
                // Saving these to return to
                params_pos: state.params_pos,
                temp_pos: state.temp_pos,
                num_params: state.num_params,
                script_number: state.script,
                ip: state.ip,
                obj: state.current_obj.id,
                remaining_selectors: selector_offsets,
            };

            let script = self.load_script(script_number);
            state.code = script.data.clone();
            state.ip = code_offset as usize;

            state.params_pos = params_pos;
            state.temp_pos = stackframe_end;
            state.num_params = num_params;
            state.script = script_number;
            Some(frame)
        }
    }

    fn call_kernel_command(&self, kernel_function: u8, params: &[Register]) -> Option<Register> {
        match kernel_function {
            0x00 => {
                // Load
                let res_type = params[1].to_i16() & 0x7F;
                let res_num = params[2].to_i16();
                info!("Kernel> Load res_type: {}, res_num: {}", res_type, res_num);
                // TODO: load it and put a "pointer" into ax -- how is it used?
            }
            0x02 => {
                // ScriptID
                let script_number = params[1].to_i16();
                let dispatch_number = if params.len() - 1 > 1 {
                    params[2].to_i16()
                } else {
                    0
                };
                info!(
                    "Kernel> ScriptID script_number: {}, dispatch_number: {}",
                    script_number, dispatch_number
                );

                let script = self.load_script(script_number as u16);
                let addr = script.get_dispatch_address(dispatch_number) as usize;
                let obj = script.get_object_by_offset(addr).unwrap();
                return Some(Register::Object(self.initialise_object(obj).id));
            }
            0x04 => {
                // Clone
                let obj = params[1].to_obj();
                todo!("This is not correct, temporary");
                // info!("Kernel> Clone obj: {}", obj.name);
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
                let flags = params[1].to_i16();
                let event = params[2].to_obj();
                todo!("how do we convert this into an object instance that we can mutate?");
                // info!("Kernel> GetEvent flags: {:x}, event: {}", flags, event.name);
                // TODO: check the events, but for now just return null event
                return Some(Register::Value(0));
            }
            0x35 => {
                // FirstNode
                // params = DblList, return Node
                // todo!("currently just return 0 for empty");
                return Some(Register::Value(0));
            }
            0x45 => {
                // Wait
                let ticks = params[1].to_i16();
                // TODO: do wait, set return value
                info!("Kernel> Wait ticks: {:x}", ticks);
                // TODO: do this for kWait
                // ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
            }
            _ => {
                debug!(
                    "Call kernel command {:x} with #params {:?}",
                    kernel_function, params
                );
                // todo!("Implement missing kernel command");
                // TODO: temp assuming it returns a value
                return Some(Register::Value(0));
            }
        }
        return None;
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
