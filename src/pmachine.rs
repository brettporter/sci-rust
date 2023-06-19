use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use chrono::{Local, Timelike};
use rand::Rng;

use elsa::FrozenMap;
use global_counter::primitive::exact::CounterUsize;
use itertools::Itertools;
use log::{debug, info};
use num_traits::FromPrimitive;

use crate::{
    events::EventManager,
    graphics::Graphics,
    picture::BackgroundState,
    resource::{self, Resource, ResourceType},
    script::{Id, Script, StringDefinition},
    view,
};

pub(crate) struct PMachine<'a> {
    resources: &'a HashMap<u16, Resource>,
    class_scripts: HashMap<u16, u16>,
    play_selector: u16,
    doit_selector: u16,
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
    Parameter,
}

// Node key / value are object registers. Can't use register type as Copy would be infinite
#[derive(Copy, Clone, Debug, PartialEq)]
struct DblListNode {
    key: Id,
    value: Id,
    list_id: usize,
    list_index: usize,
}

struct Heap {
    // TODO: should we have all these "caches", or have a single Heap?
    dbllist_cache: HashMap<usize, VecDeque<DblListNode>>,
    script_local_variables: HashMap<u16, Vec<Register>>,
}
impl Heap {
    fn new() -> Heap {
        Heap {
            dbllist_cache: HashMap::new(),
            script_local_variables: HashMap::new(),
        }
    }

    fn get_dbllist(&mut self, list_ptr: usize) -> &mut VecDeque<DblListNode> {
        self.dbllist_cache.get_mut(&list_ptr).unwrap()
    }
}

struct MachineRegisters {
    ax: Register,
    // TODO: does this belong in execution context?
    rest_modifier: usize,
    last_wait_time: Instant,
}

struct ExecutionContext {
    current_obj: Id,
    script: u16, // This will be the execution script, which may differ from the current object
    ip: usize,
    code: Box<Vec<u8>>, // TODO: look for ways to just use the script data rather than cloning, should this just be current_obj data and ip offset modified?

    stack_params: usize, // index into the stack where current params are stored
    stack_temps: usize,  // index into the stack where current temps are stored
    stack_unwind: usize, // index to unwind the stack to - usually params, but temps if there are further selectors to execute
}

impl ExecutionContext {
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

    fn num_params(&self) -> u16 {
        (self.stack_temps - self.stack_params) as u16
    }

    fn read_u16_or_u8(&mut self, cmd: u8) -> u16 {
        if cmd & 1 == 0 {
            self.read_u16()
        } else {
            self.read_u8() as u16
        }
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

    fn to_string(&self) -> (Id, usize) {
        match *self {
            Register::String(id, offset) => (id, offset),
            _ => panic!("Register was not a string {:?}", self),
        }
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

    fn sub(&mut self, dec: i16) {
        *self = match *self {
            Register::Value(v) => Register::Value(v - dec),
            _ => panic!("Register was not a value {:?}", self),
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

        // TODO: only needed for play method, doit method and debugging
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

        let (&doit_selector, _) = selector_names
            .iter()
            .find(|(_, &name)| name == "doit")
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
            doit_selector,
            script_cache: FrozenMap::new(),
            object_cache: FrozenMap::new(),
            string_cache: FrozenMap::new(),
            start_time: Instant::now(),
        }
    }

    fn load_game_object(&self) -> &ObjectInstance {
        // load the play method from the game object (at exports[0]) in script 000
        let init_script = self.initialise_script(SCRIPT_MAIN).unwrap();

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

    fn run(
        &self,
        run_object: &ObjectInstance,
        selector: u16,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        let mut stack: Vec<Register> = Vec::new();

        let mut heap = Heap::new();

        let mut reg = MachineRegisters {
            ax: Register::Undefined,
            rest_modifier: 0,
            last_wait_time: Instant::now(),
        };

        // Pre-load objects for all classes to avoid lazy init problems
        // TODO: may not need to retain class_scripts afterwards, consider simplifying
        // TODO: does this cover every script or need to enumerate resources?

        for script_number in self.resources.iter().filter_map(|(_, r)| {
            if r.resource_type == ResourceType::Script {
                Some(r.resource_number)
            } else {
                None
            }
        }) {
            if let Some(script) = self.initialise_script(script_number) {
                heap.script_local_variables
                    .insert(script_number, init_script_local_variables(script));
            }
        }

        for class_num in self.class_scripts.keys() {
            if let Some(def) = self.get_class_definition(*class_num) {
                self.initialise_object(def);
            }
        }

        self.send_selector(
            run_object,
            run_object,
            selector,
            0,
            &mut reg,
            &mut stack,
            &mut heap,
            graphics,
            event_manager,
        );
        // unwind the stack
        debug!("Unwinding stack to 0 at end of run");
        stack.truncate(0);
    }

    fn send_selector(
        &self,
        obj: &ObjectInstance,
        send_obj: &ObjectInstance,
        selector: u16,
        stack_params: usize,
        reg: &mut MachineRegisters,
        stack: &mut Vec<Register>,
        heap: &mut Heap,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        let (script_number, code_offset) = send_obj.get_func_selector(selector);

        debug!(
            "Found selector {} with code offset {:x?} in script {}",
            selector, code_offset, script_number
        );

        let s = self.get_script(script_number);

        let mut call_ctx = ExecutionContext {
            current_obj: obj.id,
            script: script_number,
            ip: code_offset as usize,
            stack_params,
            stack_temps: stack.len(),
            stack_unwind: stack.len(),
            code: s.data.clone(), // TODO: avoid clone
        };

        self.execute(&mut call_ctx, reg, stack, heap, graphics, event_manager);
    }

    // TODO: look for ways to reduce parameter passing
    fn execute(
        &self,
        ctx: &mut ExecutionContext,
        reg: &mut MachineRegisters,
        stack: &mut Vec<Register>,
        heap: &mut Heap,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        // TODO: consistent debug logging through here
        // TODO: log symbols so we can more easily debug it = opcodes, variables, selectors, classes etc.
        // TODO: we could use a separate kernel from the VM that has access to the services (graphics, events) but for now all in here
        loop {
            // TODO: break this out into a method and ensure there are good unit tests for the behaviours (e.g. the issues with num_params being wrong for call methods)
            let cmd = ctx.read_u8();
            debug!("[{}@{:x}] Executing {:x}", ctx.script, ctx.ip - 1, cmd);
            // TODO: do we do constants for opcodes? Do we enumberate the B / W variants or add tooling for this?
            // TODO: can we simplify all the unwrapping
            match cmd {
                0x02 | 0x03 => {
                    // add
                    reg.ax.add_reg(stack.pop().unwrap());
                }
                0x04 | 0x05 => {
                    // sub
                    // TODO: do we want methods like add for register to trim these?
                    reg.ax = Register::Value(stack.pop().unwrap().to_i16() - reg.ax.to_i16());
                }
                0x06 | 0x07 => {
                    // mul
                    reg.ax = Register::Value(stack.pop().unwrap().to_i16() * reg.ax.to_i16());
                }
                0x08 | 0x09 => {
                    // div
                    reg.ax = Register::Value(stack.pop().unwrap().to_i16() / reg.ax.to_i16());
                }
                0xa | 0xb => {
                    if !reg.ax.is_zero_or_null() {
                        reg.ax = Register::Value(stack.pop().unwrap().to_i16() % reg.ax.to_i16());
                    }
                }
                0x0c | 0x0d => {
                    // shr
                    reg.ax = Register::Value(stack.pop().unwrap().to_i16() >> reg.ax.to_i16());
                }
                0x12 | 0x13 => {
                    // and
                    reg.ax = Register::Value(stack.pop().unwrap().to_i16() & reg.ax.to_i16());
                }
                0x14 | 0x15 => {
                    // or
                    reg.ax = Register::Value(stack.pop().unwrap().to_i16() | reg.ax.to_i16());
                }
                0x18 | 0x19 => {
                    // not
                    reg.ax = Register::Value(if reg.ax.is_zero_or_null() { 1 } else { 0 });
                }
                0x1a | 0x1b => {
                    // eq?
                    reg.ax = Register::Value(if reg.ax == stack.pop().unwrap() { 1 } else { 0 });
                }
                0x1c | 0x1d => {
                    // ne?
                    reg.ax = Register::Value(if reg.ax != stack.pop().unwrap() { 1 } else { 0 });
                }
                0x1e | 0x1f => {
                    // gt?
                    reg.ax = Register::Value(if stack.pop().unwrap().to_i16() > reg.ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x20 | 0x21 => {
                    // ge?
                    reg.ax = Register::Value(if stack.pop().unwrap().to_i16() >= reg.ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x22 | 0x23 => {
                    // lt?
                    reg.ax = Register::Value(if stack.pop().unwrap().to_i16() < reg.ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x24 | 0x25 => {
                    // le?
                    reg.ax = Register::Value(if stack.pop().unwrap().to_i16() <= reg.ax.to_i16() {
                        1
                    } else {
                        0
                    });
                }
                0x2e => {
                    // bt W
                    let pos = ctx.read_i16();
                    if reg.ax.to_i16() != 0 {
                        ctx.jump(pos);
                    }
                }
                0x30 => {
                    // bnt W
                    let pos = ctx.read_i16();
                    if reg.ax.is_zero_or_null() {
                        ctx.jump(pos);
                    }
                }
                0x32 => {
                    // jmp W
                    let pos = ctx.read_i16();
                    ctx.jump(pos);
                }
                0x34 => {
                    // ldi W
                    reg.ax = Register::Value(ctx.read_i16());
                }
                0x35 => {
                    // ldi B
                    reg.ax = Register::Value(ctx.read_i8() as i16);
                }
                0x36 => {
                    // push
                    stack.push(reg.ax);
                }
                0x38 => {
                    // pushi W
                    stack.push(Register::Value(ctx.read_i16()));
                }
                0x39 => {
                    // pushi B
                    stack.push(Register::Value(ctx.read_i8() as i16));
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
                    let num_variables = ctx.read_u8();
                    for _ in 0..num_variables {
                        stack.push(Register::Undefined);
                    }
                }
                0x40 => {
                    // call W relpos, B framesize
                    let rel_pos = ctx.read_i16();

                    let stackframe_size = ctx.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start =
                        stackframe_end - (stackframe_size / 2 + 1 + reg.rest_modifier);
                    if reg.rest_modifier > 0 {
                        // Update number of parameters within the call stack frame
                        stack[stackframe_start].add(reg.rest_modifier as i16);
                        reg.rest_modifier = 0;
                    }

                    let code = &ctx.code;
                    let mut call_ctx = ExecutionContext {
                        stack_params: stackframe_start, // argc is included
                        stack_temps: stack.len(),
                        stack_unwind: stackframe_start,
                        code: code.clone(), // TODO: remove
                        ..*ctx
                    };
                    call_ctx.jump(rel_pos);
                    self.execute(&mut call_ctx, reg, stack, heap, graphics, event_manager);
                }
                0x43 => {
                    // callk B
                    let k_func = ctx.read_u8();
                    let k_params = ctx.read_u8() as usize / 2;
                    let stackframe_start = stack.len() - (k_params + 1 + reg.rest_modifier);
                    if reg.rest_modifier > 0 {
                        stack[stackframe_start].add(reg.rest_modifier as i16);
                        reg.rest_modifier = 0;
                    }

                    assert_eq!(stack[stackframe_start].to_i16(), k_params as i16);

                    // dump_stack(&stack, &state);

                    // call command, put return value into ax
                    if let Some(value) = self.call_kernel_command(
                        k_func,
                        stackframe_start,
                        reg,
                        stack,
                        heap,
                        graphics,
                        event_manager,
                    ) {
                        reg.ax = value;
                    }

                    // todo!("Temporary - currently just setting this to quit so it doesn't infinite loop");
                    if k_func == 0x45 {
                        if let Some(quit) = event_manager.poll() {
                            if quit {
                                heap.script_local_variables.get_mut(&SCRIPT_MAIN).unwrap()[4] =
                                    Register::Value(1);
                            }
                        }
                    }

                    // unwind stack
                    debug!("Unwinding stack to {stackframe_start} after Kernel function");
                    stack.truncate(stackframe_start);
                }
                0x45 => {
                    // callb B dispindex, B framesize
                    let dispatch_index = ctx.read_u8() as u16;
                    let stackframe_size = ctx.read_u8() as usize;
                    self.call_dispatch_index(
                        ctx,
                        SCRIPT_MAIN,
                        dispatch_index,
                        stackframe_size,
                        stack,
                        reg,
                        heap,
                        graphics,
                        event_manager,
                    );
                }
                0x46 => {
                    // calle W script, W dispindex, B framesize
                    let script_num = ctx.read_u16();
                    let dispatch_index = ctx.read_u16();
                    let stackframe_size = ctx.read_u8() as usize;

                    self.call_dispatch_index(
                        ctx,
                        script_num,
                        dispatch_index,
                        stackframe_size,
                        stack,
                        reg,
                        heap,
                        graphics,
                        event_manager,
                    );
                }
                0x48 | 0x49 => {
                    // ret
                    debug!("Return from function -> {}@{:x}", ctx.script, ctx.ip);

                    // unwind stack to previous state
                    debug!("Unwinding stack to {}", ctx.stack_unwind);
                    stack.truncate(ctx.stack_unwind);
                    return;
                }
                0x4a | 0x4b | 0x54 | 0x55 | 0x57 => {
                    // TODO: factor out a method and make this separate again. Curently hard with local variables in here.
                    let current_obj = self.get_object(ctx.current_obj);
                    let (obj, send_obj) = if cmd == 0x54 || cmd == 0x55 {
                        // self B selector
                        (current_obj, current_obj)
                    } else if cmd == 0x57 {
                        // super B class B stackframe
                        let class_num = ctx.read_u8() as u16;
                        let class = self.get_class_definition(class_num).unwrap();
                        (current_obj, self.get_object(class.id))
                    } else {
                        // send B
                        let obj = self.get_object(reg.ax.to_obj());
                        (obj, obj)
                    };

                    // TODO: instead of just pushing onto an execution stack and looping
                    // it would be good to start a new context like run so we can just pop the whole thing on return

                    let stackframe_size = ctx.read_u8() as usize;
                    let stackframe_end = stack.len();
                    let stackframe_start =
                        stackframe_end - (stackframe_size / 2 + reg.rest_modifier);

                    // dump_stack(&stack, &state);

                    let mut read_selectors_idx = 0;
                    let mut selector_offsets = VecDeque::new();
                    while read_selectors_idx < stackframe_end - stackframe_start {
                        let np = stack[stackframe_start + read_selectors_idx + 1].to_u16();
                        selector_offsets.push_back(read_selectors_idx);
                        read_selectors_idx += np as usize + 2 + reg.rest_modifier;
                        if reg.rest_modifier > 0 {
                            // Only support rest modifier for one selector, otherwise behaviour is undefined
                            assert!(read_selectors_idx == stackframe_end - stackframe_start);
                            // add rest_modifier to number of parameters on the stack
                            stack[stackframe_start + 1].add(reg.rest_modifier as i16);
                            reg.rest_modifier = 0;
                        }
                    }

                    debug!(
                        "Sending to selectors {:x?} for {} to {}",
                        selector_offsets
                            .iter()
                            .map(|o| stack[stackframe_start + o].to_u16())
                            .collect_vec(),
                        obj.name,
                        send_obj.name
                    );

                    for offset in &selector_offsets {
                        let start = stackframe_start + offset;
                        let selector = stack[start].to_u16();
                        assert_eq!(stackframe_end, stack.len());
                        self.send_to_selector(
                            obj,
                            send_obj,
                            selector,
                            start + 1,
                            reg,
                            stack,
                            heap,
                            graphics,
                            event_manager,
                        );
                    }

                    // unwind the parameters as well
                    debug!("Unwinding stack to {stackframe_start} after all selectors {:x?} for {} to {}",
                        selector_offsets.iter().map(|o| stack[stackframe_start + o].to_u16()).collect_vec(),
                        obj.name,
                        send_obj.name);
                    stack.truncate(stackframe_start);
                }
                0x51 => {
                    // class B
                    let class_num = ctx.read_u8() as u16;
                    let class = self.get_class_definition(class_num).unwrap();
                    let obj = self.get_object(class.id);

                    // TODO: do we need to change script?
                    reg.ax = Register::Object(obj.id);
                }
                0x59 => {
                    // &rest B paramindex
                    let index = ctx.read_u8() as usize;
                    let rest = &stack[ctx.stack_params + index..ctx.stack_temps];
                    reg.rest_modifier = rest.len();
                    *stack = [&stack, rest].concat();
                }
                0x5b => {
                    // lea B type, B index
                    let var_type = ctx.read_i8();
                    let mut var_index = ctx.read_u8() as i16;

                    // TODO: use bitflags
                    let use_acc = (var_type & 0b10000) != 0;
                    let var_type_num = var_type & 0b110 >> 1;

                    if use_acc {
                        var_index += reg.ax.to_i16();
                        assert!(var_index >= 0);
                    }

                    let variable_type: VariableType =
                        FromPrimitive::from_u16(var_type_num as u16).unwrap();

                    // TODO: confirm that this is correct - get the variable "address", not the value
                    reg.ax = Register::Variable(variable_type, var_index);
                }
                0x5c | 0x5d => {
                    // selfID
                    reg.ax = Register::Object(ctx.current_obj);
                }
                0x63 => {
                    // pToa B offset
                    let offset = ctx.read_u8();
                    debug!("property @offset {offset} to acc for {:?}", ctx.current_obj);
                    reg.ax = self
                        .get_object(ctx.current_obj)
                        .get_property_by_offset(offset);
                }
                0x65 => {
                    // aTop B offset
                    let offset = ctx.read_u8();
                    debug!("acc to property @offset {offset} for {:?}", ctx.current_obj);
                    self.get_object(ctx.current_obj)
                        .set_property_by_offset(offset, reg.ax);
                }
                0x67 => {
                    // pTos B offset
                    let offset = ctx.read_u8();
                    debug!("property @offset {offset} to stack");
                    stack.push(
                        self.get_object(ctx.current_obj)
                            .get_property_by_offset(offset),
                    );
                }
                0x6b => {
                    // ipToa B offset
                    let offset = ctx.read_u8();
                    debug!("increment property @offset {offset} to acc");
                    reg.ax = self
                        .get_object(ctx.current_obj)
                        .get_property_by_offset(offset);
                    reg.ax.add(1);
                    self.get_object(ctx.current_obj)
                        .set_property_by_offset(offset, reg.ax);
                }
                0x6d => {
                    // dpToa B offset
                    let offset = ctx.read_u8();
                    debug!("increment property @offset {offset} to acc");
                    reg.ax = self
                        .get_object(ctx.current_obj)
                        .get_property_by_offset(offset);
                    reg.ax.sub(1);
                    self.get_object(ctx.current_obj)
                        .set_property_by_offset(offset, reg.ax);
                }
                0x72 => {
                    // lofsa W
                    let offset = ctx.read_i16();
                    debug!("Load offset {} to acc", offset);
                    assert!(ctx.ip <= i16::MAX as usize); // Make sure this isn't a bad cast

                    // Need to check what it is at this address
                    // TODO: can we generalise what the script gives back by a type?
                    let v = (ctx.ip as i16 + offset) as usize;
                    let script = self.get_script(ctx.script);
                    reg.ax = if let Some(obj) = script.get_object_by_offset(v) {
                        Register::Object(self.initialise_object(obj).id)
                    } else if let Some(s) = script.get_string_by_offset(v) {
                        Register::String(self.initialise_string(s).id, 0)
                    } else if let Some(s) = script.get_get_said_by_offset(v) {
                        // todo!("support 'said'");
                        Register::Address(ctx.script, s.id.offset)
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
                    stack.push(Register::Object(ctx.current_obj));
                }
                // TODO: generalise this to all types 0x80..0xff
                0x81 => {
                    // lag B
                    let var = ctx.read_u8() as usize;
                    debug!("load global {} to acc", var);
                    reg.ax = heap.script_local_variables[&SCRIPT_MAIN][var];
                }
                0x85 => {
                    // lat B
                    let var = ctx.read_u8();
                    debug!("load temp {} to acc", var);
                    reg.ax = stack[ctx.stack_temps + var as usize];
                }
                0x87 => {
                    // lap B
                    let var = ctx.read_u8() as u16;
                    debug!("load parameter {} to acc", var);

                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    if var <= ctx.num_params() {
                        reg.ax = stack[ctx.stack_params + var as usize];
                    }
                }
                0x89 => {
                    // lsg B
                    let var = ctx.read_u8() as usize;
                    debug!("load global {} to stack", var);
                    stack.push(heap.script_local_variables[&SCRIPT_MAIN][var]);
                }
                0x8b => {
                    // lsl B
                    let var = ctx.read_u8();
                    debug!("load local {} to stack", var);
                    stack.push(heap.script_local_variables.get(&ctx.script).unwrap()[var as usize]);
                }
                0x8d => {
                    // lst B
                    let var = ctx.read_u8();
                    debug!("load temp {} to stack", var);
                    stack.push(stack[ctx.stack_temps + var as usize]);
                }
                0x8f => {
                    // lsp B
                    let var = ctx.read_u8() as u16;
                    debug!("load parameter {} to stack", var);
                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    if var <= ctx.num_params() {
                        stack.push(stack[ctx.stack_params + var as usize]);
                    }
                }
                0x97 => {
                    // lapi B
                    let var = ctx.read_u8() as u16 + reg.ax.to_u16();
                    debug!("load parameter {} to acc", var);
                    // TODO: It'd be nice if the stack frame didn't permit this so we don't have to check
                    if var <= ctx.num_params() {
                        reg.ax = stack[ctx.stack_params + var as usize];
                    }
                }
                0x98 | 0x99 => {
                    // lsgi W | B
                    let var = ctx.read_u16_or_u8(cmd) + reg.ax.to_u16();
                    debug!("load global {} to stack", var);
                    stack.push(heap.script_local_variables[&SCRIPT_MAIN][var as usize]);
                }
                0x9f => {
                    // lspi B
                    let var = ctx.read_u8() as u16 + reg.ax.to_u16();
                    debug!("load parameter {} to stack", var);
                    stack.push(stack[ctx.stack_params + var as usize]);
                }
                0xa0 | 0xa1 => {
                    // sag W | B
                    let var = ctx.read_u16_or_u8(cmd);
                    debug!("store accumulator to global {} = {:?}", var, reg.ax);
                    heap.script_local_variables.get_mut(&SCRIPT_MAIN).unwrap()[var as usize] =
                        if var == 223 && reg.ax.is_zero_or_null() {
                            // TODO: workaround, vsync and current rendering sets howFast to slow
                            Register::Value(2)
                        } else {
                            reg.ax
                        };
                }
                0xa3 => {
                    // sal B
                    let var = ctx.read_u8();
                    debug!("store accumulator to local {}", var);
                    heap.script_local_variables.get_mut(&ctx.script).unwrap()[var as usize] =
                        reg.ax;
                }
                0xa5 => {
                    // sat B
                    let var = ctx.read_u8() as usize;
                    debug!("store accumulator to temp {}", var);
                    stack[ctx.stack_temps + var] = reg.ax;
                }
                0xa7 => {
                    // sap B
                    let var = ctx.read_u8() as usize;
                    debug!("store accumulator to param {}", var);
                    stack[ctx.stack_params + var] = reg.ax;
                }
                0xb0 | 0xb1 => {
                    // sagi W | B
                    let var = ctx.read_u16_or_u8(cmd);
                    let idx = var + reg.ax.to_u16();
                    debug!("store accumulator {:?} to global {}", reg.ax, idx);
                    heap.script_local_variables.get_mut(&SCRIPT_MAIN).unwrap()[idx as usize] =
                        reg.ax;
                }
                0xb2 | 0xb3 => {
                    // sali W | B
                    let var = ctx.read_u16_or_u8(cmd);
                    let idx = var + reg.ax.to_u16();
                    debug!("store accumulator {:?} to local {}", reg.ax, idx);
                    heap.script_local_variables.get_mut(&ctx.script).unwrap()[idx as usize] =
                        reg.ax;
                }
                0xc1 => {
                    // +ag B
                    let var = ctx.read_u8() as usize;
                    debug!("increment global {} and store in accumulator", var);
                    heap.script_local_variables.get_mut(&SCRIPT_MAIN).unwrap()[var].add(1);
                    reg.ax = heap.script_local_variables[&SCRIPT_MAIN][var];
                }
                0xc5 => {
                    // +at B
                    let var = ctx.read_u8() as usize;
                    let idx = ctx.stack_temps + var;
                    debug!("increment temp {} and store in accumulator", var);
                    let v = stack[idx].to_i16() + 1;
                    stack[idx] = Register::Value(v);
                    reg.ax = stack[idx];
                }
                0xc9 => {
                    // +sg B
                    let var = ctx.read_u8() as usize;
                    debug!("increment global {} and store on stack", var);
                    heap.script_local_variables.get_mut(&SCRIPT_MAIN).unwrap()[var].add(1);
                    stack.push(heap.script_local_variables[&SCRIPT_MAIN][var]);
                }
                0xe1 => {
                    // -ag B
                    let var = ctx.read_u8() as usize;
                    debug!("decrement global {} and store in accumulator", var);
                    heap.script_local_variables.get_mut(&SCRIPT_MAIN).unwrap()[var].sub(1);
                    reg.ax = heap.script_local_variables[&SCRIPT_MAIN][var];
                }
                0xe5 => {
                    // -at B
                    let var = ctx.read_u8() as usize;
                    debug!("decrement temp {} and store in accumulator", var);
                    let idx = ctx.stack_temps + var;
                    let v = stack[idx].to_i16() - 1;
                    stack[idx] = Register::Value(v);
                    reg.ax = stack[idx];
                }
                0xe7 => {
                    // -ap B
                    let var = ctx.read_u8() as usize;
                    debug!("decrement param {} and store in accumulator", var);
                    let idx = ctx.stack_params + var;
                    let v = stack[idx].to_i16() - 1;
                    stack[idx] = Register::Value(v);
                    reg.ax = stack[idx];
                }
                _ => {
                    todo!("Unknown command 0x{:x}", cmd);
                }
            }
        }
    }

    fn get_inherited_var_selectors(&self, obj_class: &crate::script::ClassDefinition) -> &Vec<u16> {
        let script_num = self.class_scripts[&obj_class.super_class];
        let script = self.initialise_script(script_num).unwrap();
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

            let script = self.initialise_script(script_num).unwrap();

            let super_class_def = script.get_class(obj_class.super_class).unwrap();
            for (k, v) in self.get_inherited_functions(super_class_def) {
                func_selectors.entry(k).or_insert(v);
            }
        }
        func_selectors
    }

    fn get_script(&self, number: u16) -> &Script {
        self.script_cache.get(&number).unwrap()
    }

    fn initialise_script(&self, number: u16) -> Option<&Script> {
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
        obj: &ObjectInstance,
        send_obj: &ObjectInstance,
        selector: u16,
        stack_params: usize,
        reg: &mut MachineRegisters,
        stack: &mut Vec<Register>,
        heap: &mut Heap,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        let num_params = stack[stack_params].to_u16();
        if obj.has_var_selector(selector) {
            // Variable
            if num_params == 0 {
                // get
                debug!("Get variable by selector {:x} on {}", selector, obj.name);
                reg.ax = obj.get_property(selector);
            } else {
                debug!(
                    "Set variable by selector {:x} on {} to {:?}",
                    selector, obj.name, reg.ax
                );
                obj.set_property(selector, stack[stack_params + 1]);
            }
        } else {
            // Function
            let params = &stack[stack_params..=stack_params + num_params as usize];
            debug!(
                "Call send on function {:x} for {} to {} params: {:?}",
                selector, obj.name, send_obj.name, params
            );

            self.send_selector(
                obj,
                send_obj,
                selector,
                stack_params,
                reg,
                stack,
                heap,
                graphics,
                event_manager,
            );
        }
    }

    fn call_kernel_command(
        &self,
        kernel_function: u8,
        stack_params: usize,
        // TODO: avoid passing all these around
        reg: &mut MachineRegisters,
        stack: &mut Vec<Register>,
        heap: &mut Heap,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) -> Option<Register> {
        let params = &stack[stack_params..];
        let num_params = params[0].to_i16();
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

                let script = self.initialise_script(script_number).unwrap();
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
                // TODO: common functions for getting optional parameters using params[0] as srgc
                // TODO: instead of -1, default should be the one last used
                let animation = if num_params >= 2 {
                    params[2].to_i16()
                } else {
                    -1
                }; // optional
                let flags = if num_params >= 3 {
                    params[3].to_i16()
                } else {
                    1
                };
                // let default_palette = params[4].to_i16(); // TODO: make optional

                info!(
                    "Kernel> DrawPic pic: {} animation: {} flags: {}", // TODO: default_palette: {}",
                    pic_number,
                    animation,
                    flags //, default_palette
                );

                graphics.background_state = BackgroundState {
                    pic_number,
                    animation,
                    clear: flags != 0,
                    pic_not_valid: 1,
                };

                // TODO: handle different animations. Currently just show instantly
                // TODO: handle flags - determines whether to clear or not
                // TODO: handle default palette

                // TODO: suggestion that it should be drawn to the "background" and use animate to bring it to the foreground

                // todo!("Needs to be drawn to the background or it'll be cleared in animate");
                // let resource =
                //     resource::get_resource(&self.resources, ResourceType::Pic, pic_number).unwrap();
                // graphics.draw_picture(resource);

                // TODO: implement this
                None
            }
            0x0b => {
                // Animate
                let list_ptr = params.get(1).unwrap_or(&Register::Value(0)); // optional
                let cycle = params.get(2).unwrap_or(&Register::Value(0)).to_i16() != 0; // optional

                graphics.clear();

                if list_ptr.is_zero_or_null() {
                    // TODO: what behaviour should there be if list is not given?
                    // Docs say: Animate will dispose lastCast (internal kernel knowledge of the cast during
                    // the previous animation cycle) and redraw the picture, if needed.
                    return None;
                }

                info!("Kernel> Animate cast: {:?} cycle: {:?}", list_ptr, cycle);

                let cast = heap
                    .get_dbllist(list_ptr.to_dbllist())
                    .iter()
                    .map(|n| self.get_object(n.value))
                    .collect_vec();

                // TODO: skip if signal frozen is set
                if cycle {
                    let stack_params = stack.len();
                    stack.push(Register::Value(0)); // set argc to 0
                    debug!("Cast: {:?}", cast.iter().map(|c| &c.name).collect_vec());
                    for c in &cast {
                        self.send_selector(
                            c,
                            c,
                            self.doit_selector,
                            stack_params,
                            reg,
                            stack,
                            heap,
                            graphics,
                            event_manager,
                        )
                    }
                    // unwind the stack
                    debug!(
                        "Unwinding stack after cycling case in Animate to {}",
                        stack_params
                    );
                    stack.truncate(stack_params);
                }

                // TODO: don't redraw every time - save to the background and animate in if pic not valid
                let resource = resource::get_resource(
                    &self.resources,
                    ResourceType::Pic,
                    graphics.background_state.pic_number,
                )
                .unwrap();
                graphics.draw_picture(resource);

                // todo!("additional animate logic"); -- there's a lot else to do here to sort, filter, etc.
                // for now, just rendering each cel as is. Currently ignoring palette, priority, signal, etc.

                // TODO: factor out this code, no way it belongs in pmachine!
                for c in &cast {
                    // TODO: better methods to do this and construct a struct
                    // TODO: define the selectors somewhere
                    let x = c.get_property(4).to_i16();
                    let y = c.get_property(3).to_i16();
                    let z = c.get_property(0x55).to_i16();
                    let view_num = c.get_property(5).to_u16();
                    let loop_num = c.get_property(6).to_i16();
                    let cel = c.get_property(7).to_i16();
                    let signal = c.get_property(0x11).to_i16();

                    debug!(
                        "Draw cel ({},{},{}) view: {:?} loop: {} cel: {} signal: {}",
                        x, y, z, view_num, loop_num, cel, signal
                    );

                    // TODO: why is magnifying glass -1 and doesn't move? (must need Bresen)
                    // TODO: do we ignore or correct negative cel?
                    // let cel = if cel < 0 { 0 } else { cel };
                    if cel >= 0 {
                        // Hide signal
                        if signal & 0x8 == 0 {
                            // TODO: improve caching of view
                            let resource = resource::get_resource(
                                self.resources,
                                ResourceType::View,
                                view_num,
                            )
                            .unwrap();
                            let view = view::load_view(&resource);
                            // TODO: when should z be set and used to adjust rect?
                            graphics.draw_view(&view, loop_num as usize, cel as usize, x, y, 0);
                        }
                    }
                }

                graphics.present();

                // No return value
                None
            }
            0x0e => {
                // NumCels
                let obj = params[1].to_obj();
                info!("Kernel> NumCels for object {:?}", obj);
                let v = self.get_object(obj);
                let view_num = v.get_property(5).to_u16();
                let loop_num = v.get_property(6).to_i16();
                // TODO: improve caching of view
                let resource =
                    resource::get_resource(self.resources, ResourceType::View, view_num).unwrap();
                let view = view::load_view(&resource);
                let num_cels = view[loop_num as usize].len() as i16;
                debug!(
                    "Number of cels for view: {} loop: {} = {}",
                    view_num, loop_num, num_cels
                );
                Some(Register::Value(num_cels))
            }
            0x1b => {
                // Display (text)

                // params[1] may be a string (from format), or a resource (in which case, params[2] is a string number)
                let (txt, next_param) = match params[1] {
                    Register::String(_, _) => (self.get_string_value(params[1]), 2),
                    Register::Value(res_num) => {
                        let txt_res = resource::get_resource(
                            &self.resources,
                            ResourceType::Text,
                            res_num as u16,
                        )
                        .unwrap();
                        let str_num = params[2].to_u16();

                        // TODO: proper parsing of text resource
                        let strings = txt_res.resource_data.split(|&x| x == 0).collect_vec();
                        let txt = std::str::from_utf8(strings[str_num as usize]).unwrap();
                        (txt, 3)
                    }
                    _ => panic!("Register was not a string or a value {:?}", params[1]),
                };

                info!("Kernel> Display {}", txt);
                // TODO: handle commands in subsequent parameters, starting from next_param
                // TODO: display text
                // TODO: handle return to &FarPtr
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
                ::std::thread::sleep(Duration::new(0, ticks as u32 * TICK_DURATION));
                let t = Instant::now();
                let result =
                    t.duration_since(reg.last_wait_time).as_nanos() / TICK_DURATION as u128;
                debug!("Time between wait {}", result);
                reg.last_wait_time = t;
                Some(Register::Value(result as i16))
            }
            0x46 => {
                // GetTime
                let mode = params.len() > 1;
                info!("Kernel> GetTime (system time?: {:?})", mode);

                if !mode {
                    // Convert millis to ticks (60 ticks/sec)
                    let ticks = self.start_time.elapsed().as_millis() * 60 / 1000;
                    // TODO: will defintely wrap after a few minutes, is that expected?
                    debug!("ticks = {} -> {}", ticks, ticks as i16);
                    Some(Register::Value(ticks as i16))
                } else {
                    let local = Local::now();
                    let t = local.hour12().1 << 12 | local.minute() << 6 | local.second();
                    debug!("time = {}", t);
                    // This will exceed i16 but the consumers only care about if the number of seconds have changed
                    // assert!(t < i16::MAX as u32);
                    Some(Register::Value(t as i16))
                }
            }
            0x49 => {
                // StrCmp
                let (s1, s2) = (
                    self.get_string_value(params[1]),
                    self.get_string_value(params[2]),
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
            0x4c => {
                // Format (string)
                // TODO: get dest from first parameter, which is also what is returned

                // TODO: duplicated with Display, refactor
                let txt_res =
                    resource::get_resource(&self.resources, ResourceType::Text, params[2].to_u16())
                        .unwrap();
                let str_num = params[3].to_u16();

                // TODO: proper parsing of text resource
                let strings = txt_res.resource_data.split(|&x| x == 0).collect_vec();
                let fmt = std::str::from_utf8(strings[str_num as usize]).unwrap();

                // TODO: read parameters for formatting
                info!("Kernel> Format {}", fmt);
                // TODO: implement this
                // todo!(): return the string not the formatting - this is just a dummy valid value
                Some(Register::String(
                    Id {
                        script_number: 99,
                        offset: 449,
                    },
                    0,
                ))
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
                let obj = params[1].to_obj();
                let step_factor = if params.len() > 2 {
                    params[2].to_i16()
                } else {
                    1
                }; // optional
                info!("Kernel> InitBresen {:?} by {}", obj, step_factor);

                let mover = self.get_object(obj);
                // TODO: support names as a way of lookup
                // client = 0x2d
                // x = 4, y = 3
                let client = self.get_object(mover.get_property(0x2d).to_obj());
                let mover_x = mover.get_property(4).to_i16();
                let mover_y = mover.get_property(3).to_i16();
                let client_x = client.get_property(4).to_i16();
                let client_y = client.get_property(3).to_i16();
                let x_step = client.get_property(0x36).to_i16();
                let y_step = client.get_property(0x37).to_i16();

                let (dx, dy) = (mover_x - client_x, mover_y - client_y);
                let x_axis = dx.abs() > dy.abs();
                let (mut step_dx, mut step_dy) = match x_axis {
                    true => (x_step * step_factor, x_step * step_factor * dy / dx),
                    false => (y_step * step_factor * dx / dy, y_step * step_factor),
                };
                if dx < 0 {
                    step_dx = -step_dx;
                }
                if dy < 0 {
                    step_dy = -step_dy;
                }

                // TODO: document this
                // Octants go counter clockwise starting with a line up and to the right with major x-axis
                let octant = match (x_axis, dx >= 0, dy >= 0) {
                    (true, true, false) => 1,
                    (false, true, false) => 2,
                    (false, false, false) => 3,
                    (true, false, false) => 4,
                    (true, false, true) => 5,
                    (false, false, true) => 6,
                    (false, true, true) => 7,
                    (true, true, true) => 8,
                };

                // i1 is added when di >= 0 (rounded up to next pixel)
                let i1 = match octant {
                    1 | 3 | 5 | 7 => 2 * (step_dy * dx - step_dx * dy),
                    2 | 4 | 6 | 8 => 2 * (step_dx * dy - step_dy * dx),
                    _ => panic!("Invalid octant"),
                };
                // i2 is added when di < 0 (rounded down to current pixel)
                // di is set to the initial value
                let (i2, di) = match octant {
                    1 | 8 => (-2 * dx, i1 - dx),
                    2 | 3 => (2 * dy, i1 + dy),
                    4 | 5 => (2 * dx, i1 + dx),
                    6 | 7 => (-2 * dy, i1 - dy),
                    _ => panic!("Invalid octant"),
                };
                // Direction of the minor axis
                let minor_axis_incr = match octant {
                    2 | 5 | 7 | 8 => 1,
                    1 | 3 | 4 | 6 => -1,
                    _ => panic!("Invalid octant"),
                };

                // TODO constrain dx if we would exceed dy

                // TODO: debug logging
                mover.set_property(0x2e, Register::Value(step_dx)); // dx
                mover.set_property(0x2f, Register::Value(step_dy)); // dy
                mover.set_property(0x31, Register::Value(i1)); // b-i1
                mover.set_property(0x32, Register::Value(i2)); // b-i2
                mover.set_property(0x33, Register::Value(di)); // b-di
                mover.set_property(0x34, Register::Value(if x_axis { 1 } else { 0 })); // b-x-axis
                mover.set_property(0x35, Register::Value(minor_axis_incr)); // b-incr

                None
            }
            0x54 => {
                // DoBresen
                let obj = params[1].to_obj();

                info!("Kernel> DoBresen {:?} ", obj);

                // TODO: remove hardcoding
                let mover = self.get_object(obj);
                let mover_x = mover.get_property(4).to_i16();
                let mover_y = mover.get_property(3).to_i16();

                let client = self.get_object(mover.get_property(0x2d).to_obj());
                let mut client_x = client.get_property(4).to_i16();
                let mut client_y = client.get_property(3).to_i16();

                let dx = mover.get_property(0x2e).to_i16();
                let dy = mover.get_property(0x2f).to_i16();
                let i1 = mover.get_property(0x31).to_i16();
                let i2 = mover.get_property(0x32).to_i16();
                let mut di = mover.get_property(0x33).to_i16();
                let x_axis = mover.get_property(0x34).to_i16() != 0;
                let minor_axis_incr = mover.get_property(0x35).to_i16();

                // TODO: handle move count / move speed variables

                // TODO: debug logging
                if (x_axis && (mover_x - client_x).abs() <= dx.abs())
                    || (!x_axis && (mover_y - client_y).abs() <= dy.abs())
                {
                    // Reached destination
                    client_x = mover_x;
                    client_y = mover_y;
                } else {
                    // Move forward one step
                    client_x += dx;
                    client_y += dy;
                    if di < 0 {
                        di += i1;
                    } else {
                        di += i2;
                        if x_axis {
                            client_y += minor_axis_incr;
                        } else {
                            client_x += minor_axis_incr;
                        }
                    }
                }
                client.set_property(4, Register::Value(client_x));
                client.set_property(3, Register::Value(client_y));

                // TODO check "can't be here"

                mover.set_property(0x31, Register::Value(i1));
                mover.set_property(0x32, Register::Value(i2));
                mover.set_property(0x33, Register::Value(di));

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
            }
        };
    }

    fn call_dispatch_index(
        &self,
        ctx: &ExecutionContext,
        script_num: u16,
        dispatch_index: u16,
        stackframe_size: usize,
        stack: &mut Vec<Register>,
        reg: &mut MachineRegisters,
        heap: &mut Heap,
        graphics: &mut Graphics,
        event_manager: &mut EventManager,
    ) {
        let stackframe_end = stack.len();
        let stackframe_start = stackframe_end - (stackframe_size / 2 + 1 + reg.rest_modifier);
        if reg.rest_modifier > 0 {
            stack[stackframe_start].add(reg.rest_modifier as i16);
            reg.rest_modifier = 0;
        }

        // Switch to script
        let script = self.get_script(script_num);

        let mut call_ctx = ExecutionContext {
            script: script_num,
            ip: script.get_dispatch_address(dispatch_index) as usize,
            stack_params: stackframe_start, // argc is included
            stack_temps: stack.len(),
            stack_unwind: stackframe_start,
            code: script.data.clone(), // TODO: remove
            ..*ctx
        };
        self.execute(&mut call_ctx, reg, stack, heap, graphics, event_manager);
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
        let script = self.initialise_script(script_number)?;
        script.get_class(class_num)
    }

    fn get_string(&self, s: Id) -> &StringDefinition {
        self.string_cache.get(&s).unwrap()
    }

    fn get_string_value(&self, s: Register) -> &str {
        // TODO: would be better if we had a ref into the string table with a slice
        // This is a bit unclear, but we're using .0 as the ID and .1 as the offset into that string, so this
        // returns the slice of the actual string from offset (which is the whole string by default)
        let s = s.to_string();
        &self.get_string(s.0).string[s.1..]
    }
}

fn init_script_local_variables(script: &Script) -> Vec<Register> {
    script
        .variables
        .iter()
        .map(|&v| {
            // TODO: remove this assertion when we are confident an i16 can be used
            // assert!(v <= i16::MAX as u16 || v == 0xffff);
            Register::Value(v as i16)
        })
        .collect_vec()
}

fn _dump_stack(stack: &Vec<Register>, ctx: &ExecutionContext) {
    // TODO: maybe show call stack too
    for i in 0..stack.len() {
        // TODO: not tracking where temp ends and these are pushing new values for send
        let indent = if i == ctx.stack_temps {
            if i != ctx.stack_params {
                "temp ==========>"
            } else {
                "params + temp =>"
            }
        } else if i == ctx.stack_params {
            "params ========>"
        } else {
            "                "
        };
        debug!("[{:0>3}] {indent} {:?}", i, stack[i]);
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
