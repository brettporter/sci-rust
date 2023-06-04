use std::ffi::CStr;

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

pub(crate) struct Script<'a> {
    // TODO: add storage for other block types
    // TODO: change all the "number" to id
    pub number: u16,
    exports: Vec<u16>,
    pub variables: Vec<u16>,
    classes: Vec<ClassDefinition>,
    objects: Vec<ObjectDefinition>,
    main_object_name: Option<u16>,
    pub data: &'a [u8],
}

struct ScriptBlock<'a> {
    block_type: ScriptBlockType,
    block_size: usize,
    block_data: &'a [u8],
}

pub(crate) struct ClassDefinition {
    pub script_number: u16, // TODO: we might want a better reference than this
    pub species: u16,
    pub super_class: u16,
    name_offset: u16,
    // TODO: better definition than this
    pub function_selectors: Vec<(u16, u16)>,
}

pub(crate) struct ObjectDefinition {
    pub id: u16,
    // TODO: nest or init by copying?
    pub class_definition: ClassDefinition,
}

impl<'a> Script<'a> {
    pub(crate) fn load(resource: &'a Resource) -> Self {
        debug!("Loading script #{}", resource.resource_number);

        let mut idx = 0;
        let data = resource.resource_data.as_slice();

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
            let block_data = &data[idx + 4..idx + block_size];
            idx += block_size; // includes header size

            debug!("Found block type {:?} size {}", &block_type, block_size);
            blocks.push(ScriptBlock {
                block_type,
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
        let mut variables = Vec::new();
        for block in blocks {
            // TODO: for rest - match on block type and store the data, e.g. exports. Parse it if possible.
            match block.block_type {
                ScriptBlockType::Object => {
                    let class_definition = parse_class_definition(&block, &resource);
                    // TODO: could show name
                    debug!(
                        "Object {} (species = {}, super class = {})",
                        objects.len(),
                        class_definition.species,
                        class_definition.super_class
                    );
                    objects.push(ObjectDefinition {
                        id: objects.len() as u16,
                        class_definition,
                    });
                }
                ScriptBlockType::Code => {
                    debug!("Code (first byte {:x})", block.block_data[0]);
                }
                ScriptBlockType::SynonymWordList => todo!(),
                ScriptBlockType::Said => {
                    debug!("Said spec (first byte {:x})", block.block_data[0]);
                }
                ScriptBlockType::Strings => {
                    // TODO: read all the strings and store them
                    let s = CStr::from_bytes_until_nul(&block.block_data)
                        .unwrap()
                        .to_str()
                        .unwrap();
                    debug!("Strings (first string {})", s);
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
                    variables = (0..block.block_data.len())
                        .step_by(2)
                        .map(|i| u16::from_le_bytes(block.block_data[i..i + 2].try_into().unwrap()))
                        .collect_vec();
                    debug!("Local variables {}", variables.len());
                }
                _ => {}
            }
        }

        // exports[0] is a reference an object
        // TODO: should this be done for other scripts as well? Script 997 has 0xfffe in it but others are correct
        let main_object_name = if resource.resource_number == 0 {
            let main_offset = exports[0] as usize;
            Some(u16::from_le_bytes(
                data[main_offset + 6..main_offset + 8].try_into().unwrap(),
            ))
        } else {
            None
        };

        Self {
            number: resource.resource_number,
            exports,
            classes,
            objects,
            variables,
            main_object_name,
            data,
        }
    }

    pub(crate) fn get_main_object(&self) -> &ObjectDefinition {
        self.objects
            .iter()
            .find(|&o| Some(o.class_definition.name_offset) == self.main_object_name)
            .unwrap()
    }

    pub(crate) fn get_class(&self, species: u16) -> &ClassDefinition {
        self.classes.iter().find(|&c| c.species == species).unwrap()
    }
}

fn parse_class_definition(block: &ScriptBlock, resource: &Resource) -> ClassDefinition {
    let magic = u16::from_le_bytes(block.block_data[0..2].try_into().unwrap());
    assert_eq!(magic, 0x1234);
    let local_vars_offset = u16::from_le_bytes(block.block_data[2..4].try_into().unwrap());
    assert_eq!(local_vars_offset, 0);
    let func_selector_offset =
        u16::from_le_bytes(block.block_data[4..6].try_into().unwrap()) as usize;
    let num_variables = u16::from_le_bytes(block.block_data[6..8].try_into().unwrap());
    let species = u16::from_le_bytes(block.block_data[8..10].try_into().unwrap());
    let super_class = u16::from_le_bytes(block.block_data[10..12].try_into().unwrap());
    let info = u16::from_le_bytes(block.block_data[12..14].try_into().unwrap());
    let name_offset = u16::from_le_bytes(block.block_data[14..16].try_into().unwrap());

    // TODO: do we need the name? should be an offset into strings table, so we could create a lookup for this. Consider CStr::from_bytes_until_nul to read asciiz.
    // let len = data[name_offset..].iter().position(|x| *x == 0).unwrap();
    // let name = std::str::from_utf8(&data[name_offset..name_offset + len]).unwrap();
    //println!("Class {name}...");

    todo!("read remaining variable selectors");

    if block.block_type == ScriptBlockType::Class {
        todo!("read selector IDs for variables if a class");
    }

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

    ClassDefinition {
        script_number: resource.resource_number,
        species,
        super_class,
        name_offset,
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
