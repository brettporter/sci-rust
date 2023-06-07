use std::{cell::RefCell, ffi::CStr};

use itertools::Itertools;
use log::debug;
use num_traits::FromPrimitive;

use crate::resource::Resource;

#[derive(FromPrimitive, PartialEq, Debug)]
#[repr(u8)]
enum ScriptBlockType {
    Terminator,
    Object,
    Code,
    SynonymWordList,
    Said,
    Strings,
    Class,
    Exports,
    RelocationTable,
    PreloadTextFlag,
    LocalVariables,
}

pub(crate) struct Script {
    // TODO: add storage for other block types
    // TODO: change all the "number" to id
    pub number: u16,
    exports: Vec<u16>,
    pub variables: RefCell<Vec<u16>>, // TODO: if we can't live with u16 then we might need to copy it into the loop
    classes: Vec<ClassDefinition>,
    objects: Vec<ClassDefinition>,
    strings: Vec<StringDefinition>,
    main_object_offset: Option<usize>,
    pub data: Box<Vec<u8>>,
}

struct ScriptBlock<'a> {
    block_type: ScriptBlockType,
    block_offset: usize,
    block_size: usize,
    block_data: &'a [u8],
}

pub(crate) struct ClassDefinition {
    pub script_number: u16, // TODO: we might want a better reference than this
    offset: usize,
    pub species: u16,
    pub super_class: u16,
    pub info: u16, // TODO: enum for type?
    pub name: String,
    pub variables: Vec<u16>,
    pub variable_selectors: Vec<u16>,
    // TODO: better definition than this
    pub function_selectors: Vec<(u16, u16)>,
}
impl ClassDefinition {
    // TODO: set on construction
    pub(crate) fn id(&self) -> usize {
        // TODO: what about clones?
        assert!(self.offset < u16::MAX as usize);
        usize::from(self.script_number) << 16 | self.offset
    }
}

pub(crate) struct StringDefinition {
    pub offset: usize,
    pub string: String,
}

impl Script {
    pub(crate) fn load(resource: &Resource) -> Self {
        debug!("Loading script #{}", resource.resource_number);

        let mut idx = 0;
        // TODO: is clone here avoidable?
        let data = resource.resource_data.clone();

        let mut blocks: Vec<ScriptBlock> = Vec::new();

        loop {
            let block_type: ScriptBlockType =
                FromPrimitive::from_u16(u16::from_le_bytes(data[idx..idx + 2].try_into().unwrap()))
                    .unwrap();
            if block_type == ScriptBlockType::Terminator {
                break;
            }

            let block_size =
                u16::from_le_bytes(data[idx + 2..idx + 4].try_into().unwrap()) as usize;
            let block_offset = idx + 4;
            let block_data = &data[block_offset..idx + block_size];
            idx += block_size; // includes header size

            debug!("Found block type {:?} size {}", &block_type, block_size);
            blocks.push(ScriptBlock {
                block_type,
                block_offset,
                block_size,
                block_data,
            });
        }

        // TODO: 2 export blocks being present
        // TODO: more effecient to construct vecs of these as we go rather than keep searching?
        let exports = parse_export_block(
            blocks
                .iter()
                .find(|b| b.block_type == ScriptBlockType::Exports),
        );

        let mut classes = Vec::new();
        let mut objects = Vec::new();
        let mut variables = RefCell::new(Vec::new());
        let mut strings = Vec::new();
        for block in blocks {
            // TODO: for rest - match on block type and store the data, e.g. exports. Parse it if possible.
            match block.block_type {
                ScriptBlockType::Object => {
                    let class_definition = parse_class_definition(&block, &resource);
                    objects.push(class_definition);
                }
                ScriptBlockType::Code => {
                    debug!("Code (first byte {:x})", block.block_data[0]);
                }
                ScriptBlockType::SynonymWordList => todo!(),
                ScriptBlockType::Said => {
                    debug!("Said spec (first byte {:x})", block.block_data[0]);
                }
                ScriptBlockType::Strings => {
                    let mut index = 0;
                    while index < block.block_data.len() {
                        let string = CStr::from_bytes_until_nul(&block.block_data[index..])
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string();

                        debug!("String: '{string}'");
                        let l = string.len() + 1;
                        strings.push(StringDefinition {
                            offset: index + block.block_offset,
                            string,
                        });
                        index += l;
                    }
                }
                ScriptBlockType::Class => {
                    let class_definition = parse_class_definition(&block, &resource);

                    classes.push(class_definition);
                }
                ScriptBlockType::RelocationTable => {
                    let num_pointers =
                        u16::from_le_bytes(block.block_data[0..2].try_into().unwrap()) as usize;
                    assert_eq!(num_pointers * 2 + 2, block.block_data.len());
                    debug!("Relocation table: {num_pointers} pointers");
                }
                ScriptBlockType::PreloadTextFlag => {
                    // TODO: indicates we must load text.XXX
                    debug!("Pre-load text (text.{:0>3})", resource.resource_number);
                    assert!(block.block_data.is_empty());
                }
                ScriptBlockType::LocalVariables => {
                    variables = RefCell::new(
                        (0..block.block_data.len())
                            .step_by(2)
                            .map(|i| {
                                u16::from_le_bytes(block.block_data[i..i + 2].try_into().unwrap())
                            })
                            .collect_vec(),
                    );
                    debug!("Local variables");
                }
                _ => {}
            }
        }

        // exports[0] is a reference an object
        // TODO: should this be done for other scripts as well? Script 997 has 0xfffe in it but others are correct
        let main_object_offset = if resource.resource_number == 0 {
            Some(exports[0] as usize)
        } else {
            None
        };

        Self {
            number: resource.resource_number,
            exports,
            classes,
            objects,
            variables,
            strings,
            main_object_offset,
            data,
        }
    }

    pub(crate) fn get_main_object(&self) -> &ClassDefinition {
        self.get_object(self.main_object_offset.unwrap())
    }

    pub(crate) fn get_class(&self, species: u16) -> &ClassDefinition {
        self.classes.iter().find(|&c| c.species == species).unwrap()
    }

    pub(crate) fn get_object(&self, offset: usize) -> &ClassDefinition {
        self.objects.iter().find(|&o| o.offset == offset).unwrap()
    }

    pub(crate) fn get_string_by_offset(&self, offset: usize) -> Option<&StringDefinition> {
        self.strings.iter().find(|&s| s.offset == offset)
    }

    pub(crate) fn get_object_by_offset(&self, offset: usize) -> Option<&ClassDefinition> {
        self.objects.iter().find(|&o| o.offset == offset)
    }
}

fn parse_class_definition(block: &ScriptBlock, resource: &Resource) -> ClassDefinition {
    let magic = u16::from_le_bytes(block.block_data[0..2].try_into().unwrap());
    assert_eq!(magic, 0x1234);
    let vars_offset = u16::from_le_bytes(block.block_data[2..4].try_into().unwrap());
    assert_eq!(vars_offset, 0);
    let func_selector_offset =
        u16::from_le_bytes(block.block_data[4..6].try_into().unwrap()) as usize;
    let num_variables = u16::from_le_bytes(block.block_data[6..8].try_into().unwrap());

    // Load variable values
    // TODO: load as mapped to selectors from the species or just an array we index into?
    let variables = (0..num_variables)
        .map(|i| {
            let offset = i as usize * 2 + 8;
            u16::from_le_bytes(block.block_data[offset..offset + 2].try_into().unwrap())
        })
        .collect_vec();
    let [species, super_class, info, name_offset] = variables[0..4] else {
        panic!("Class definition without all Obj variables: {:?}", variables);
    };

    // TODO: do we need the name? should be an offset into strings table, so we could create a lookup for this instead of parsing again
    let name = CStr::from_bytes_until_nul(&resource.resource_data[name_offset as usize..])
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    debug!("Parsing {:?} {name}", block.block_type);

    let variable_selectors = if block.block_type == ScriptBlockType::Class {
        let selector_data =
            &block.block_data[8 + num_variables as usize * 2..8 + num_variables as usize * 4];
        (0..selector_data.len())
            .step_by(2)
            .map(|offset| u16::from_le_bytes(selector_data[offset..offset + 2].try_into().unwrap()))
            .collect_vec()
    } else {
        // TODO: do we want to get them from the super class if it's already defined? Or fill it in later?
        Vec::new()
    };

    let num_functions = u16::from_le_bytes(
        block.block_data[func_selector_offset + 6..func_selector_offset + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    let func_table_offset = func_selector_offset + 8;
    let code_table_offset = func_table_offset + 2 + num_functions * 2;
    let function_selectors = (0..num_functions)
        .map(|i| {
            let index = i * 2;
            let selector = u16::from_le_bytes(
                block.block_data[func_table_offset + index..func_table_offset + index + 2]
                    .try_into()
                    .unwrap(),
            );
            let code_offset = u16::from_le_bytes(
                block.block_data[code_table_offset + index..code_table_offset + index + 2]
                    .try_into()
                    .unwrap(),
            );

            (selector, code_offset)
        })
        .collect_vec();

    // TODO: species, super_class, name_offset are duplicated, would it be better to add helper methods to class definition into the variables and then remove the destructuring in here?
    ClassDefinition {
        script_number: resource.resource_number,
        offset: block.block_offset + 8,
        species,
        super_class,
        info,
        name,
        variables,
        variable_selectors,
        function_selectors,
    }
}

fn parse_export_block(block: Option<&ScriptBlock>) -> Vec<u16> {
    match block {
        Some(b) => {
            let data = b.block_data;
            let num_exports = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
            assert_eq!(data.len(), num_exports * 2 + 2);

            (1..=num_exports)
                .map(|i| u16::from_le_bytes(data[i * 2..i * 2 + 2].try_into().unwrap()))
                .collect_vec()
        }
        None => Vec::new(),
    }
}
