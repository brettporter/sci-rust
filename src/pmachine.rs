use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use rand::Rng;

use elsa::FrozenMap;
use global_counter::primitive::exact::CounterUsize;
use itertools::Itertools;
use log::{debug, info};
use num_traits::FromPrimitive;

use crate::{
    events::EventManager,
    graphics::Graphics,
    resource::{self, Resource, ResourceType},
    script::{Id, Script, StringDefinition},
};

pub(crate) struct PMachine<'a> {
    resources: &'a HashMap<u16, Resource>,
    class_scripts: HashMap<u16, u16>,
    play_selector: u16,
    script_cache: FrozenMap<u16, Box<Script>>,

    // TODO: move to heap?
    object_cache: FrozenMap<Id, Box<ObjectInstance>>,
    string_cache: FrozenMap<Id, Box<StringDefinition>>,

    start_time: Instant,
}

#[derive(FromPrimitive, Copy, Clone, Debug, PartialEq)]
enum VariableType {
    Global,
    Local,
    Temporary,
    Paramter,
}

// Node key / value are object registers. Can't use register type as Copy would be infinite
#[derive(Copy, Clone, Debug, PartialEq)]
struct DblListNode {
    key: Id,
    value: Id,
    list_id: usize,
    list_index: usize,
}

struct StackFrame {
    // Unwind position
    unwind_pos: usize,

    // Restore values of the parent caller
    params_pos: usize,
    num_params: u16,
    temp_pos: usize,
    stack_len: usize,

    // Code point to return to
    script_number: u16,
    ip: usize,
    obj: Id,

    // Any selectors not yet called in the last send
    remaining_selectors: VecDeque<usize>,
}

struct Heap {
    // TODO: should we have all these "caches", or have a single Heap?
    dbllist_cache: HashMap<usize, VecDeque<DblListNode>>,
}
impl Heap {
    fn new() -> Heap {
        Heap {
            dbllist_cache: HashMap::new(),
        }
    }

    fn get_dbllist(&mut self, list_ptr: usize) -> &mut VecDeque<DblListNode> {
        self.dbllist_cache.get_mut(&list_ptr).unwrap()
    }
}

// TODO: remove lifecycle by owning the bits of object instance needed for state
// and looking up the rest when used
// TODO: probably separate the registers from the heap but don't want to pass too many things around
struct MachineState {
    ip: usize, // instruction pointer
    // TODO: should this just be current_obj data and ip offset modified?
    // Don't need code stored here as we can look it up from the script
    code: Box<Vec<u8>>, // currently executing script data
    current_obj: Id,
    ax: Register,

    // Stack information
    // TODO: better approach than this to create a frame?
    params_pos: usize,
    temp_pos: usize,
    num_params: u16,
    rest_modifier: usize,
    script: u16,
}

impl MachineState {
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

    fn update_script(&mut self, script: &Script) {
        self.script = script.number;
        self.code = script.data.clone(); // TODO: remove clone
    }
}

// TODO: should probably be bitflags instead in the script reader, and then just defining the type of object/clone/class in here as needed
#[derive(Debug, FromPrimitive)]
enum ObjectType {
    Object,
    Clone = 1,
    Class = 0x8000,
}

// Counter for clones. Starting value ensures no overlap with existing IDs
// Note: because we are cloning an event every tick this will grow fast, but we have more than enough space in a usize
static CLONE_COUNTER: CounterUsize = CounterUsize::new(1);
static DBLLIST_COUNTER: CounterUsize = CounterUsize::new(1);

#[derive(Debug)]
struct ObjectInstance {
    id: Id,
    name: String,
    species: u16,
    object_type: ObjectType,
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

    fn kernel_clone(&self) -> Box<ObjectInstance> {
        // Currently using a global counter for all clones
        let id = CLONE_COUNTER.inc();
        // TODO: spec says selectors should be empty - but it might just mean in the memory space and can still look up the inherited ones,
        // or perhaps it calls on a different object with a current object pointer to the clone. May be ok to clone selectors here but re-evaluate if problems.

        // TODO: can avoid cloning variables/selectors?
        const CLONE_SPACE: u16 = 1000;
        let instance = ObjectInstance {
            id: Id::new(CLONE_SPACE, id),
            name: self.name.clone(), // TODO: modify to represent clone?
            object_type: ObjectType::Clone,
            species: self.species,
            variables: self.variables.clone(),
            var_selectors: self.var_selectors.clone(),
            func_selectors: self.func_selectors.clone(),
        };
        Box::new(instance)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Register {
    Value(i16),
    Object(Id),
    Variable(VariableType, i16),
    String(Id, usize),   // TODO: second field being an offset isn't very clear
    Address(u16, usize), // TODO: a bit redundant - this matches up to ID in script
    DblList(usize),
    Node(DblListNode),
    Undefined,
}
impl Register {
    fn to_i16(&self) -> i16 {
        match *self {
            Register::Value(v) => v,
            _ => panic!("Register was not a value {:?}", self),
        }
    }

    fn to_obj(&self) -> Id {
        match *self {
            Register::Object(v) => v,
            _ => panic!("Register was not an object {:?}", self),
        }
    }
    fn is_obj(&self) -> bool {
        match *self {
            Register::Object(_) => true,
            _ => false,
        }
    }

    fn to_dbllist(&self) -> usize {
        match *self {
            Register::DblList(v) => v,
            _ => panic!("Register was not a dbllist {:?}", self),
        }
    }

    fn to_node(&self) -> DblListNode {
        match *self {
            Register::Node(n) => n,
            _ => panic!("Register was not a node {:?}", self),
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
            Register::Object(_v) => false, // some operations check it is not 0, but we don't yet have a use case for defining it that way
            Register::DblList(v) => v == 0,
            Register::Node(_) => false, // Nodes can be checked against null but are never null (in that case a Value(0) was returned instead) -- TODO: do we make it nullable somehow instead?
            _ => panic!("Register {:?} doesn't have a zero value", *self),
        }
    }

    fn add_reg(&mut self, inc: Register) {
        *self = match *self {
            Register::Value(v) => match inc {
                Register::Value(v2) => Register::Value(v + v2),
                Register::String(v2, offset) => Register::String(v2, offset + v as usize), // TODO: no bounds checking on length of string here
                _ => panic!("Second register was not able to be added {:?}", inc),
            },
            _ => panic!("Register was not a value {:?}", self),
        }
    }

    fn add(&mut self, inc: i16) {
        *self = match *self {
            Register::Value(v) => Register::Value(v + inc),
            _ => panic!("Register was not a value {:?}", self),
        }
    }

    fn to_string(&self) -> (Id, usize) {
        match *self {
            Register::String(id, offset) => (id, offset),
            _ => panic!("Register was not a string {:?}", self),
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
            string_cache: FrozenMap::new(),
            start_time: Instant::now(),
        }
    }

    fn load_game_object(&self) -> &ObjectInstance {
        // load the play method from the game object (at exports[0]) in script 000
        let init_script = self.load_script(SCRIPT_MAIN).unwrap();

        self.initialise_object(init_script.get_main_object())
    }

    fn initialise_object(&self, obj: &crate::script::ClassDefinition) -> &ObjectInstance {
        if let Some(o) = self.object_cache.get(&obj.id) {
            return o;
        }

        // TODO: ways of avoiding clone? We can move the class def into here
        let var_selectors = if obj.info == ObjectType::Class as u16 {
            // Class
            obj.variable_selectors.clone()
        } else {
            // Object
            self.get_inherited_var_selectors(&obj).clone()
        };

        let instance = ObjectInstance {
            id: obj.id,
            name: String::from(&obj.name),
            species: obj.species,
            object_type: FromPrimitive::from_u16(obj.info & !0x4).unwrap(), // TODO: what does the 0x4 represent?
            variables: RefCell::new(
                obj.variables
                    .iter()
                    .map(|&v| {
                        // TODO: check case. Script 995 has a value that is either 32768 or -32768
                        // Perhaps -1 and i16::MAX are special cases?
                        // assert!(v <= i16::MAX as u16);
                        Register::Value(v as i16)
                    })
                    .collect_vec(),
            ),
            var_selectors,
            func_selectors: self.get_inherited_functions(&obj),
        };
        self.object_cache.insert(obj.id, Box::new(instance))
    }

    fn initialise_string(&self, s: &crate::script::StringDefinition) -> &StringDefinition {
        if let Some(s) = self.string_cache.get(&s.id) {
            return s;
        }
        // TODO: just a copy, is this necessary
        let instance = StringDefinition {
            id: s.id,
            string: s.string.clone(),
        };
        self.string_cache.insert(s.id, Box::new(instance))
    }

    pub(crate) fn run_game_play_method(
        &self,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        let game_object = self.load_game_object();

        self.run(game_object, self.play_selector, graphics, event_manager);
    }

    // TODO: consistent debug logging through here
    // TODO: log symbols so we can more easily debug it = opcodes, variables, selectors, classes etc.
    // TODO: we could use a separate kernel from the VM that has access to the services (graphics, events) but for now all in here
    fn run(
        &self,
        run_object: &ObjectInstance,
        selector: u16,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        let mut stack: Vec<Register> = Vec::new();

        let mut call_stack: Vec<StackFrame> = Vec::new();

        let (script_number, code_offset) = run_object.get_func_selector(selector);

        debug!(
            "Found selector {} with code offset {:x?} in script {}",
            selector, code_offset, script_number
        );

        // TODO: better to do this by execution a machine function? Not happy with passing all the info in
        // TODO: separate registers and stack frames, can keep some of this out of the send to selector function which is just creating a call
        let s = self.load_script(script_number).unwrap();

        let mut state = MachineState {
            code: s.data.clone(), // TODO: remove clone
            ip: code_offset as usize,
            current_obj: run_object.id,
            ax: Register::Undefined,
            params_pos: 0,
            num_params: 0,
            temp_pos: 0,
            rest_modifier: 0,
            script: script_number,
        };

        let mut heap = Heap::new();

        // Pre-load objects for all classes to avoid lazy init problems
        for class_num in self.class_scripts.keys() {
            if let Some(def) = self.get_class_definition(*class_num) {
                self.initialise_object(def);
            }
        }

        // TODO: get variables from all loaded scripts, rather than loading again
        let mut global_vars: Vec<Register> = self
            .load_script(SCRIPT_MAIN)
            .unwrap()
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
            // TODO: can we simplify all the unwrapping
            match cmd {
                0x02 | 0x03 => {
                    // add
                    state.ax.add_reg(stack.pop().unwrap());
                }
                0x04 | 0x05 => {
                    // sub
                    // TODO: do we want methods like add for register to trim these?
                    state.ax = Register::Value(stack.pop().unwrap().to_i16() - state.ax.to_i16());
                }
                0x06 | 0x07 => {
                    // mul
                    state.ax = Register::Value(stack.pop().unwrap().to_i16() * state.ax.to_i16());
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
                    state.ax = Register::Value(if state.ax.is_zero_or_null() { 1 } else { 0 });
                }
                0x1a | 0x1b => {
                    // eq?
                    state.ax = Register::Value(if state.ax == stack.pop().unwrap() {
                        1
                    } else {
                        0
                    });
                }
                0x1c | 0x1d => {
                    // ne?
                    state.ax = Register::Value(if state.ax != stack.pop().unwrap() {
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
                0x20 | 0x21 => {
                    // ge?
                    state.ax =
                        Register::Value(if stack.pop().unwrap().to_i16() >= state.ax.to_i16() {
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
                    // todo!(): this may not be correct - in some cases it will be a single string of some length.
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
                    let stackframe_start =
                        stackframe_end - (stackframe_size / 2 + 1 + state.rest_modifier);
                    if state.rest_modifier > 0 {
                        stack[stackframe_start].add(state.rest_modifier as i16);
                        state.rest_modifier = 0;
                    }

                    call_stack.push(StackFrame {
                        // Unwind position
                        unwind_pos: stackframe_start,
                        // Saving these to return to
                        params_pos: state.params_pos,
                        temp_pos: state.temp_pos,
                        num_params: state.num_params,
                        stack_len: stackframe_end,
                        script_number: state.script,
                        ip: state.ip,
                        obj: state.current_obj,
                        remaining_selectors: VecDeque::new(),
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
                    let stackframe_start = stack.len() - (k_params + 1 + state.rest_modifier);
                    if state.rest_modifier > 0 {
                        stack[stackframe_start].add(state.rest_modifier as i16);
                        state.rest_modifier = 0;
                    }
                    let params = &stack[stackframe_start..];

                    let num_params = params[0].to_i16();
                    assert_eq!(num_params, k_params as i16);

                    // dump_stack(&stack, &state);

                    // call command, put return value into ax
                    if let Some(value) =
                        self.call_kernel_command(&mut state, &mut heap, graphics, k_func, params)
                    {
                        state.ax = value;
                    }

                    // todo!("Temporary - currently just setting this to quit so it doesn't infinite loop");
                    if k_func == 0x45 {
                        if let Some(quit) = event_manager.poll() {
                            if quit {
                                global_vars[4] = Register::Value(1);
                            }
                        }
                    }

                    // unwind stack
                    stack.truncate(stackframe_start);
                }
                0x45 => {
                    // callb B dispindex, B framesize
                    let dispatch_index = state.read_u8() as u16;
                    let stackframe_size = state.read_u8() as usize;
                    self.call_dispatch_index(
                        SCRIPT_MAIN,
                        dispatch_index,
                        stackframe_size,
                        &mut stack,
                        &mut state,
                        &mut call_stack,
                    );
                }
                0x46 => {
                    // calle W script, W dispindex, B framesize
                    let script_num = state.read_u16();
                    let dispatch_index = state.read_u16();
                    let stackframe_size = state.read_u8() as usize;

                    self.call_dispatch_index(
                        script_num,
                        dispatch_index,
                        stackframe_size,
                        &mut stack,
                        &mut state,
                        &mut call_stack,
                    );
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

                    let previous_obj = self.get_object(state.current_obj);
                    let script = self.load_script(frame.script_number).unwrap();
                    state.update_script(script);
                    state.ip = frame.ip;
                    state.current_obj = frame.obj;

                    state.params_pos = frame.params_pos;
                    state.temp_pos = frame.temp_pos;
                    state.num_params = frame.num_params;
                    // unwind stack to previous state
                    stack.truncate(frame.stack_len);

                    let mut running_function = false;
                    let remaining_selectors = &mut frame.remaining_selectors.clone(); // TODO: is clone needed?
                    while !remaining_selectors.is_empty() {
                        let obj = previous_obj;
                        let start = frame.unwind_pos + remaining_selectors.pop_front().unwrap();
                        let end = stack.len();
                        let selector = stack[start].to_u16();
                        let pos = start + 1;
                        let np = stack[pos].to_u16();

                        // dump_stack(&stack, &state);

                        if let Some(frame) = self.send_to_selector(
                            &mut state,
                            &stack[pos..=pos + np as usize],
                            obj,
                            obj, // TODO: is this right for self with multiple selectors?
                            selector,
                            np,
                            frame.unwind_pos,
                            end,
                            pos,
                            remaining_selectors.clone(), // TODO: is clone needed again?
                        ) {
                            state.current_obj = obj.id;
                            call_stack.push(frame);
                            running_function = true;
                            break;
                        }
                    }
                    if !running_function {
                        // Unwind stack as ret will not be called
                        stack.truncate(frame.unwind_pos);
                    }
                }
                0x4a | 0x4b | 0x54 | 0x55 | 0x57 => {
                    // TODO: factor out a method and make this separate again. Curently hard with local variables in here.
                    let current_obj = self.get_object(state.current_obj);
                    let (obj, send_obj) = if cmd == 0x54 || cmd == 0x55 {
                        // self B selector
                        (current_obj, current_obj)
                    } else if cmd == 0x57 {
                        // super B class B stackframe
                        let class_num = state.read_u8() as u16;
                        let class = self.get_class_definition(class_num).unwrap();
                        (current_obj, self.get_object(class.id))
                    } else {
                        // send B
                        let obj = self.get_object(state.ax.to_obj());
                        (obj, obj)
                    };

                    // TODO: instead of just pushing onto an execution stack and looping
                    // it would be good to start a new context like run so we can just pop the whole thing on return

                    let stackframe_size = state.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start =
                        stackframe_end - (stackframe_size / 2 + state.rest_modifier);

                    // dump_stack(&stack, &state);

                    let mut read_selectors_idx = 0;
                    let mut selector_offsets = VecDeque::new();
                    while read_selectors_idx < stackframe_end - stackframe_start {
                        let np = stack[stackframe_start + read_selectors_idx + 1].to_u16();
                        selector_offsets.push_back(read_selectors_idx);
                        read_selectors_idx += np as usize + 2 + state.rest_modifier;
                        if state.rest_modifier > 0 {
                            // Only support rest modifier for one selector, otherwise behaviour is undefined
                            assert!(read_selectors_idx == stackframe_end - stackframe_start);
                            // add rest_modifier to number of parameters on the stack
                            stack[stackframe_start + 1].add(state.rest_modifier as i16);
                            state.rest_modifier = 0;
                        }
                    }

                    debug!(
                        "Sending to selectors {:?} for {} to {}",
                        selector_offsets, obj.name, send_obj.name
                    );
                    while !selector_offsets.is_empty() {
                        let start = stackframe_start + selector_offsets.pop_front().unwrap();
                        let selector = stack[start].to_u16();
                        let pos = start + 1;
                        let np = stack[pos].to_u16();
                        if let Some(frame) = self.send_to_selector(
                            &mut state,
                            &stack[pos..=pos + np as usize],
                            obj,
                            send_obj,
                            selector,
                            np,
                            stackframe_start,
                            stackframe_end,
                            pos,
                            selector_offsets.clone(), // TODO: is clone needed?
                        ) {
                            state.current_obj = obj.id;
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
                    let class = self.get_class_definition(class_num).unwrap();
                    let obj = self.get_object(class.id);

                    // TODO: do we need to change script?
                    state.ax = Register::Object(obj.id);
                }
                0x59 => {
                    // &rest B paramindex
                    let index = state.read_u8() as usize;
                    let rest = &stack[state.params_pos + index..state.temp_pos];
                    state.rest_modifier = rest.len();
                    stack = [&stack, rest].concat();
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
                    state.ax = Register::Object(state.current_obj);
                }
                0x63 => {
                    // pToa B offset
                    let offset = state.read_u8();
                    debug!(
                        "property @offset {offset} to acc for {:?}",
                        state.current_obj
                    );
                    state.ax = self
                        .get_object(state.current_obj)
                        .get_property_by_offset(offset);
                }
                0x65 => {
                    // aTop B offset
                    let offset = state.read_u8();
                    debug!(
                        "acc to property @offset {offset} for {:?}",
                        state.current_obj
                    );
                    self.get_object(state.current_obj)
                        .set_property_by_offset(offset, state.ax);
                }
                0x67 => {
                    // pTos B offset
                    let offset = state.read_u8();
                    debug!("property @offset {offset} to stack");
                    stack.push(
                        self.get_object(state.current_obj)
                            .get_property_by_offset(offset),
                    );
                }
                0x6b => {
                    // ipToa B offset
                    let offset = state.read_u8();
                    debug!("increment property @offset {offset} to acc");
                    state.ax = self
                        .get_object(state.current_obj)
                        .get_property_by_offset(offset);
                    state.ax.add(1);
                    self.get_object(state.current_obj)
                        .set_property_by_offset(offset, state.ax);
                }
                0x72 => {
                    // lofsa W
                    let offset = state.read_i16();
                    debug!("Load offset {} to acc", offset);
                    assert!(state.ip <= i16::MAX as usize); // Make sure this isn't a bad cast

                    // Need to check what it is at this address
                    // TODO: can we generalise what the script gives back by a type?
                    let v = (state.ip as i16 + offset) as usize;
                    let script = self.load_script(state.script).unwrap();
                    state.ax = if let Some(obj) = script.get_object_by_offset(v) {
                        Register::Object(self.initialise_object(obj).id)
                    } else if let Some(s) = script.get_string_by_offset(v) {
                        Register::String(self.initialise_string(s).id, 0)
                    } else if let Some(s) = script.get_get_said_by_offset(v) {
                        // todo!("support 'said'");
                        Register::Address(state.script, s.id.offset)
                    } else {
                        // TODO: may need to put a whole lot of handles into script?
                        todo!("Unknown method loading from address {:x}", v);
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
                0x8b => {
                    // lsl B
                    let var = state.read_u8();
                    debug!("load local {} to stack", var);
                    // TODO: local variable boilerplate could be reduced, can they have a pointer in the state somewhere like data?
                    let script = self.load_script(state.script).unwrap();
                    stack.push(Register::Value(
                        script.variables.borrow()[var as usize] as i16,
                    ));
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
                0x9f => {
                    // lspi B
                    let var = state.read_u8() as u16 + state.ax.to_u16();
                    debug!("load parameter {} to stack", var);
                    stack.push(stack[state.params_pos + var as usize]);
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
                    let script = self.load_script(state.script).unwrap();
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
                0xc1 => {
                    // +ag B
                    let var = state.read_u8() as usize;
                    global_vars[var].add(1);
                    state.ax = global_vars[var];
                }
                0xc5 => {
                    // +at B
                    let var = state.read_u8() as usize;
                    let idx = state.temp_pos + var;
                    let v = stack[idx].to_i16() + 1;
                    stack[idx] = Register::Value(v);
                    state.ax = stack[idx];
                }
                0xe5 => {
                    // -at B
                    let var = state.read_u8() as usize;
                    let idx = state.temp_pos + var;
                    let v = stack[idx].to_i16() - 1;
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
        let script = self.load_script(script_num).unwrap();
        let super_class_def = script.get_class(obj_class.super_class).unwrap();
        &super_class_def.variable_selectors
    }

    fn get_inherited_functions(
        &self,
        obj_class: &crate::script::ClassDefinition,
    ) -> HashMap<u16, (u16, u16)> {
        let mut func_selectors: HashMap<u16, (u16, u16)> = HashMap::new();

        for (s, offset) in &obj_class.function_selectors {
            func_selectors.insert(*s, (obj_class.id.script_number, *offset));
        }

        if obj_class.super_class != NO_SUPER_CLASS {
            let script_num = self.class_scripts[&obj_class.super_class];

            let script = self.load_script(script_num).unwrap();

            let super_class_def = script.get_class(obj_class.super_class).unwrap();
            for (k, v) in self.get_inherited_functions(super_class_def) {
                func_selectors.entry(k).or_insert(v);
            }
        }
        func_selectors
    }

    fn load_script(&self, number: u16) -> Option<&Script> {
        if let Some(script) = self.script_cache.get(&number) {
            return Some(script);
        }
        let script = Script::load(resource::get_resource(
            &self.resources,
            ResourceType::Script,
            number,
        )?);

        Some(self.script_cache.insert(number, Box::new(script)))
    }

    fn send_to_selector(
        &self,
        state: &mut MachineState,
        params: &[Register],
        obj: &ObjectInstance,
        send_obj: &ObjectInstance,
        selector: u16,
        num_params: u16,
        // TODO: remove these params
        stackframe_start: usize,
        stackframe_end: usize,
        params_pos: usize,
        selector_offsets: VecDeque<usize>,
    ) -> Option<StackFrame> {
        if obj.has_var_selector(selector) {
            // Variable
            if num_params == 0 {
                // get
                debug!("Get variable by selector {:x} on {}", selector, obj.name);
                state.ax = obj.get_property(selector);
            } else {
                debug!(
                    "Set variable by selector {:x} on {} to {:?}",
                    selector, obj.name, state.ax
                );
                obj.set_property(selector, params[1]);
            }
            None
        } else {
            // Function
            debug!(
                "Call send on function {:x} for {} to {} params: {:?}",
                selector, obj.name, send_obj.name, params
            );
            let (script_number, code_offset) = send_obj.get_func_selector(selector);
            debug!("    -> {script_number} @{:x}", code_offset);

            let frame = StackFrame {
                // Unwind position
                unwind_pos: stackframe_start,
                // Saving these to return to
                params_pos: state.params_pos,
                temp_pos: state.temp_pos,
                num_params: state.num_params,
                stack_len: stackframe_end,
                script_number: state.script,
                ip: state.ip,
                obj: state.current_obj,
                remaining_selectors: selector_offsets,
            };

            let script = self.load_script(script_number).unwrap();
            state.update_script(script);
            state.ip = code_offset as usize;

            state.params_pos = params_pos;
            state.temp_pos = stackframe_end;
            state.num_params = num_params;
            Some(frame)
        }
    }

    fn call_kernel_command(
        &self,
        state: &mut MachineState,
        heap: &mut Heap,
        graphics: &mut Graphics, // TODO: avoid passing this around...
        kernel_function: u8,
        params: &[Register],
    ) -> Option<Register> {
        return match kernel_function {
            0x00 => {
                // Load
                let res_type = params[1].to_i16() & 0x7F;
                let res_num = params[2].to_i16();
                info!("Kernel> Load res_type: {}, res_num: {}", res_type, res_num);
                // todo!(): load it and put a "pointer" into ax -- how is it used?
                None
            }
            0x02 => {
                // ScriptID
                let script_number = params[1].to_u16();
                let dispatch_number = if params.len() > 2 {
                    params[2].to_u16()
                } else {
                    0
                };
                info!(
                    "Kernel> ScriptID script_number: {}, dispatch_number: {}",
                    script_number, dispatch_number
                );

                let script = self.load_script(script_number).unwrap();
                let addr = script.get_dispatch_address(dispatch_number) as usize;
                if let Some(obj) = script.get_object_by_offset(addr) {
                    Some(Register::Object(self.initialise_object(obj).id))
                } else {
                    Some(Register::Address(script_number, addr))
                }
            }
            0x03 => {
                // DisposeScript
                // TODO: do we bother?
                let script_number = params[1].to_i16();
                info!("Kernel> Dispose script {}", script_number);
                None
            }
            0x04 => {
                // Clone
                // TODO: wrap all the direct uses of object_cache and dbllist_cache
                let obj = self.get_object(params[1].to_obj());
                let id = self.clone_object(obj);
                info!("Kernel> Clone obj: {} to {:?}", obj.name, id);
                Some(Register::Object(id))
            }
            0x05 => {
                // DisposeClone
                let id = params[1].to_obj();
                info!("Kernel> Dispose Clone obj: {:?}", id);
                self.dispose_clone(id);
                None
            }
            0x06 => {
                // IsObject
                let is_obj = params[1].is_obj();
                info!("Kernel> IsObject {:?} = {}", params[1], is_obj);
                Some(Register::Value(if is_obj { 1 } else { 0 }))
            }
            0x08 => {
                // DrawPic
                let pic_number = params[1].to_u16();
                let animation = params[2].to_i16(); // TODO: make optional
                let flags = params[3].to_i16(); // TODO: make optional
                let default_palette = params[4].to_i16(); // TODO: make optional

                info!(
                    "Kernel> DrawPic pic: {} animation: {} flags: {} default_palette: {}",
                    pic_number, animation, flags, default_palette
                );

                // TODO: handle different animations. Currently just show instantly
                // TODO: handle flags - determines whether to clear or not
                // TODO: handle default palette

                // TODO: suggestion that it should be drawn to the "background" and use animate to bring it to the foreground

                let resource =
                    resource::get_resource(&self.resources, ResourceType::Pic, pic_number).unwrap();
                graphics.render_resource(resource);

                // TODO: implement this
                None
            }
            0x0b => {
                // Animate
                let list_ptr = params.get(1).unwrap_or(&Register::Value(0)); // optional
                let cycle = params.get(2).unwrap_or(&Register::Value(0)).to_i16() != 0; // optional

                if list_ptr.is_zero_or_null() {
                    // TODO: what behaviour should there be if list is not given?
                    return None;
                }

                // TODO: if background picture not drawn, animate with style from kDrawPic
                info!("Kernel> Animate cast: {:?} cycle: {:?}", list_ptr, cycle);

                let cast = heap.dbllist_cache.get(&list_ptr.to_dbllist()).unwrap();

                // TODO: map to objects?
                for c in cast {
                    let o = self.get_object(c.value);
                    debug!("{:?}", o);
                }

                // TODO: call doit if cycle is set

                // todo!("animate");

                // No return value
                None
            }
            0x1c => {
                // GetEvent
                let flags = params[1].to_i16();
                let event = self.get_object(params[2].to_obj());
                info!("Kernel> GetEvent flags: {:x}, event: {}", flags, event.name);
                // TODO: check the events, but for now just return null event
                Some(Register::Value(0))
            }
            0x20 => {
                // DrawMenuBar
                let mode = params[1].to_i16();
                info!("Kernel> Draw menu bar mode: {mode}");
                // TODO: draw it
                None
            }
            0x22 => {
                // AddMenu
                info!("Kernel> AddMenu");
                // TODO: add menu
                None
            }
            0x26 => {
                // SetSynonyms
                info!("Kernel> SetSynonyms");
                // TODO: set synonyms
                None
            }
            0x27 => {
                // HaveMouse
                info!("Kernel> HaveMouse");
                // TODO: add support for mouse
                Some(Register::Value(0))
            }
            0x28 => {
                // SetCursor
                let resource = params[1].to_u16();
                let visible = !params[2].is_zero_or_null();
                let (y, x) = (params.get(3), params.get(4)); // optional
                info!(
                    "Kernel> SetCursor {:x} {} ({:?}, {:?})",
                    resource, visible, x, y
                );

                // TODO: move/show/hide/change the cursor

                None
            }
            0x30 => {
                // GameIsRestarting
                info!("Kernel> Game is restarting?");
                // TODO: handle when it is
                Some(Register::Value(0))
            }
            0x31 => {
                // DoSound
                info!("Kernel> Do sound");
                // TODO: complicated subset based on action - for now just return 0
                Some(Register::Value(0))
            }
            0x32 => {
                // NewList
                info!("Kernel> NewList");
                let id = DBLLIST_COUNTER.inc();

                // Using VecDeque because we can't access the underlying prev/next of a node in LinkedList anyway
                // So we'll do this by searching and indexing even if that is O(n)
                heap.dbllist_cache.insert(id, VecDeque::new());
                Some(Register::DblList(id))
            }
            0x33 => {
                // DisposeList
                let list_ptr = params[1].to_dbllist();
                info!("Kernel> DisposeList {}", list_ptr);
                heap.dbllist_cache.remove(&list_ptr);
                None
            }
            0x34 => {
                // NewNode
                let value = params[1].to_obj();
                let key = params[2].to_obj();
                info!("Kernel> NewNode v: {:x?} k: {:x?}", value, key);
                Some(Register::Node(DblListNode {
                    key,
                    value,
                    // TODO: this is gross, can we do better with links?
                    list_id: 0,
                    list_index: 0,
                }))
            }
            0x35 => {
                // FirstNode
                let list_ptr = params[1].to_dbllist();
                if list_ptr == 0 {
                    return Some(Register::Value(0));
                }
                info!("Kernel> FirstNode {:x}", list_ptr);
                let list = heap.dbllist_cache.get(&list_ptr).unwrap();

                if let Some(&first_node) = list.front() {
                    Some(Register::Node(first_node))
                } else {
                    Some(Register::Value(0))
                }
            }
            0x38 => {
                // NextNode
                let node = params[1].to_node();
                info!("Kernel> NextNode {:?}", node);

                let list = heap.dbllist_cache.get(&node.list_id).unwrap();
                if let Some(&n) = list.get(node.list_index + 1) {
                    Some(Register::Node(n))
                } else {
                    Some(Register::Value(0))
                }
            }
            0x3a => {
                // NodeValue
                let node = params[1].to_node();
                info!("Kernel> NodeValue {:?}", node);
                Some(Register::Object(node.value))
            }
            0x3c => {
                // AddToFront
                let list_ptr = params[1].to_dbllist();
                if list_ptr == 0 {
                    return Some(Register::Value(0));
                }
                // Update the list information
                let node = DblListNode {
                    list_id: list_ptr,
                    list_index: 0,
                    ..params[2].to_node()
                };
                info!("Kernel> AddToFront {:x} {:?}", list_ptr, node);

                let list = heap.get_dbllist(list_ptr);
                list.push_front(node);
                None
            }
            0x3d => {
                // AddToEnd
                let list_ptr = params[1].to_dbllist();
                if list_ptr == 0 {
                    return Some(Register::Value(0));
                }
                let list = heap.get_dbllist(list_ptr);

                // Update the list information
                let node = DblListNode {
                    list_id: list_ptr,
                    list_index: list.len(),
                    ..params[2].to_node()
                };
                info!("Kernel> AddToEnd {:x} {:?}", list_ptr, node);

                list.push_back(node);
                None
            }
            0x3e => {
                // FindKey
                let list_ptr = params[1].to_dbllist();
                if list_ptr == 0 {
                    return Some(Register::Value(0));
                }
                // Is keyed by an object - TODO: check if other types are also used
                let key = params[2].to_obj();

                info!("Kernel> FindKey {:x?} in {:x}", key, list_ptr);
                let list = heap.dbllist_cache.get(&list_ptr).unwrap();
                if let Some(&node) = list.iter().find(|&n| n.key == key) {
                    Some(Register::Node(node))
                } else {
                    Some(Register::Value(0))
                }
            }
            0x3f => {
                let list_ptr = params[1].to_dbllist();
                if list_ptr == 0 {
                    return Some(Register::Value(0));
                }
                let key = params[2].to_obj();
                info!("Kernel> DeleteKey {:x?} in {:x}", key, list_ptr);
                let list = heap.dbllist_cache.get_mut(&list_ptr).unwrap();
                if let Some((pos, _)) = list.iter().find_position(|&n| n.key == key) {
                    list.remove(pos);
                }
                None
            }
            0x40 => {
                // Random
                let (min, max) = (params[1].to_i16(), params[2].to_i16());
                info!("Kernel> Random {}..={}", min, max);
                Some(Register::Value(rand::thread_rng().gen_range(min..=max)))
            }
            0x43 => {
                // GetAngle
                info!("Kernel> GetAngle");
                // TODO: implement
                Some(Register::Value(0))
            }
            0x45 => {
                // Wait
                let ticks = params[1].to_i16();
                info!("Kernel> Wait ticks: {:x}", ticks);
                const TICK_DURATION: u32 = 1_000_000_000u32 / 60;
                // TODO: currently 0 a lot, is that correct?
                ::std::thread::sleep(Duration::new(0, ticks as u32 * TICK_DURATION));
                // TODO: set return value
                Some(Register::Value(0))
            }
            0x46 => {
                // GetTime
                let mode = params.len() > 1;
                info!("Kernel> GetTime (system time?: {:?})", mode);

                // TODO: what about not present
                if !mode {
                    // Convert millis to ticks (60 ticks/sec)
                    let ticks = self.start_time.elapsed().as_millis() * 60 / 1000;
                    // TODO: will defintely wrap after a few minutes, is that expected?
                    debug!("ticks = {} -> {}", ticks, ticks as i16);
                    Some(Register::Value(ticks as i16))
                } else {
                    todo!("Implement GetTime {:?}", mode);
                }
            }
            0x49 => {
                // StrCmp
                let (s1, s2) = (params[1].to_string(), params[2].to_string());
                // We have some that are indexed into the string, so can't lookup by offset - get straight from data

                // TODO: would be better if we had a ref into the string table with a slice
                // This is a bit unclear, but we're using .0 as the ID and .1 as the offset into that string, so this
                // returns the slice of the actual string from offset (which is the whole string by default)
                let (s1, s2) = (
                    &self.get_string(s1.0).string[s1.1..],
                    &self.get_string(s2.0).string[s2.1..],
                );

                let ord = if params.len() > 3 {
                    let n = params[3].to_i16() as usize;
                    s1[..n].cmp(&s2[..n])
                } else {
                    s1.cmp(s2)
                };
                info!("Kernel> StrCmp {}, {}", s1, s2);
                Some(Register::Value(match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            0x4f => {
                // BaseSetter
                info!("Kernel> BaseSetter");
                // TODO implement
                None
            }
            0x50 => {
                // DirLoop
                info!("Kernel> DirLoop");
                // TODO implement
                None
            }
            0x51 => {
                // CanBeHere
                let obj = params[1].to_obj();
                // TODO: support optional parameter of a clip list (DblList) -- assert if the rest is working
                info!("Kernel> Can be here: {:?}", obj);
                // TODO: infinite loop unless we do this properly. For now always succeed.
                return Some(Register::Value(1));
            }
            0x53 => {
                // InitBresen
                info!("Kernel> InitBresen");
                // TODO implement
                None
            }
            0x60 => {
                // SetMenu
                info!("Kernel> SetMenu");
                // TODO
                None
            }
            0x62 => {
                // GetCWD
                let address = params[1];
                info!("Kernel> GetCWD into {:?}", address);
                // TODO: write a string to the address
                return Some(address);
            }
            0x68 => {
                // GetSaveDir
                info!("Kernel> GetSaveDir");
                // TODO: return a string
                Some(Register::Undefined)
            }
            0x6b => {
                // FlushResources
                info!("Kernel> FlushResources");
                // TODO: implement
                Some(Register::Value(0))
            }
            0x70 => {
                // Graph
                info!("Kernel> Graph");
                // TODO: implement
                Some(Register::Value(0))
            }
            _ => {
                debug!(
                    "Call kernel command {:x} with #params {:?}",
                    kernel_function, params
                );
                todo!("Implement missing kernel command {:x}", kernel_function);
                // TODO: temp assuming it returns a value
                Some(Register::Value(0))
            }
        };
    }

    fn call_dispatch_index(
        &self,
        script_num: u16,
        dispatch_index: u16,
        stackframe_size: usize,
        stack: &mut [Register],
        state: &mut MachineState,
        call_stack: &mut Vec<StackFrame>,
    ) {
        let stackframe_end = stack.len();
        let stackframe_start = stackframe_end - (stackframe_size / 2 + 1 + state.rest_modifier);
        if state.rest_modifier > 0 {
            stack[stackframe_start].add(state.rest_modifier as i16);
            state.rest_modifier = 0;
        }

        call_stack.push(StackFrame {
            // Unwind position
            unwind_pos: stackframe_start,
            // Saving these to return to
            params_pos: state.params_pos,
            temp_pos: state.temp_pos,
            num_params: state.num_params,
            stack_len: stack.len(),
            script_number: state.script,
            ip: state.ip,
            obj: state.current_obj,
            remaining_selectors: VecDeque::new(),
        });

        // As opposed to send, does not start with selector
        state.num_params = stack[stackframe_start].to_u16();

        // Switch to script
        let script = self.load_script(script_num).unwrap();
        state.update_script(script);
        state.ip = script.get_dispatch_address(dispatch_index) as usize;

        state.params_pos = stackframe_start; // argc is included
        state.temp_pos = stackframe_end;
    }

    fn get_object(&self, id: Id) -> &ObjectInstance {
        self.object_cache.get(&id).unwrap()
    }

    fn clone_object(&self, obj: &ObjectInstance) -> Id {
        let clone = obj.kernel_clone();
        assert!(self.object_cache.get(&clone.id).is_none());
        self.object_cache.insert(clone.id, clone).id
    }

    fn dispose_clone(&self, id: Id) {
        // todo!(): stuck on mutability here
        // self.object_cache.as_mut().remove(&id);
    }

    fn get_class_definition(&self, class_num: u16) -> Option<&crate::script::ClassDefinition> {
        let script_number = self.class_scripts[&class_num];
        let script = self.load_script(script_number)?;
        script.get_class(class_num)
    }

    fn get_string(&self, s: Id) -> &StringDefinition {
        self.string_cache.get(&s).unwrap()
    }
}

fn dump_stack(stack: &Vec<Register>, state: &MachineState) {
    // TODO: maybe show call stack too
    for i in 0..stack.len() {
        // TODO: not tracking where temp ends and these are pushing new values for send
        let indent = if i == state.temp_pos {
            "temp ===>"
        } else if i == state.params_pos {
            "params =>"
        } else {
            "         "
        };
        debug!("{indent} {:?}", stack[i]);
    }
    debug!("num_params: {}", state.num_params);
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
