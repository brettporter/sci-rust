// TODO: Integration tests for this using game data that isn't checked in, unit tests for this module

use std::io::Cursor;

use bitstream_io::{BigEndian, BitRead, BitReader};

// SCI0 compression types
#[derive(FromPrimitive, PartialEq)]
pub(crate) enum CompressionType {
    None,
    LZW,
    Huffman,
}

enum HuffmanNode {
    Tree(Box<HuffmanNode>, Box<HuffmanNode>),
    Leaf(u8),
    Literal(),
}

fn build_huffman_node(node_data: &[u8], node_index: u8) -> HuffmanNode {
    let offset = node_index as usize * 2;
    let value = node_data[offset];
    let siblings = node_data[offset + 1];
    match siblings {
        // Value
        0 => HuffmanNode::Leaf(value),
        _ => {
            let left = siblings >> 4;
            let right = siblings & 0xF;
            HuffmanNode::Tree(
                Box::new(build_huffman_node(node_data, left + node_index)),
                match right {
                    // Literal
                    0 => Box::new(HuffmanNode::Literal()),
                    // Node
                    _ => Box::new(build_huffman_node(node_data, right + node_index)),
                },
            )
        }
    }
}

pub(crate) fn huffman_decode(compressed_data: Vec<u8>) -> Vec<u8> {
    let num_nodes = compressed_data[0] as usize;
    let terminator = compressed_data[1];

    // TODO: this is vulnerable to bad data causing infinite loop, etc.

    // Can't use a traditional Huffman encoding library as none seem to support reading a byte from compressed data when no encoding is present

    // Build tree
    let node_data = &compressed_data[2..num_nodes * 2 + 2];
    let tree = build_huffman_node(node_data, 0);

    let mut result: Vec<u8> = Vec::new();
    let mut reader = BitReader::endian(
        Cursor::new(&compressed_data[num_nodes * 2 + 2..]),
        BigEndian,
    );

    let mut node = &tree;

    loop {
        node = match node {
            HuffmanNode::Leaf(value) => {
                result.push(*value);
                &tree
            }
            HuffmanNode::Literal() => {
                let literal = reader.read(8).unwrap();
                if literal == terminator {
                    return result;
                }
                result.push(literal);
                &tree
            }
            HuffmanNode::Tree(left, right) => {
                if reader.read_bit().unwrap() {
                    right
                } else {
                    left
                }
            }
        };
    }
}
