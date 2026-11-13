//! Preflight for the flat FP32 ONNX exports supported by StillWatch. The field
//! numbers follow ONNX's onnx.proto. Prost reads the wire format so we can inspect
//! the graph while borrowing the weight buffers that will be passed to ORT.

use prost::encoding::{WireType, decode_key, decode_varint};

use crate::InferenceError;

pub(crate) const OPSET: u32 = 17;
const IR_VERSION: u64 = 8;
const INVALID: InferenceError = InferenceError::ModelGraph;

enum Field<'a> {
    Integer(u64),
    Bytes(&'a [u8]),
    Float,
}

fn next_field<'a>(data: &mut &'a [u8]) -> Result<Option<(u32, Field<'a>)>, InferenceError> {
    if data.is_empty() {
        return Ok(None);
    }
    let (tag, wire) = decode_key(data).map_err(|_| INVALID)?;
    let value = match wire {
        WireType::Varint => Field::Integer(decode_varint(data).map_err(|_| INVALID)?),
        WireType::LengthDelimited => {
            let len = decode_varint(data).map_err(|_| INVALID)?;
            let len = usize::try_from(len).map_err(|_| INVALID)?;
            let (bytes, rest) = data.split_at_checked(len).ok_or(INVALID)?;
            *data = rest;
            Field::Bytes(bytes)
        }
        WireType::ThirtyTwoBit => {
            *data = data.get(4..).ok_or(INVALID)?;
            Field::Float
        }
        _ => return Err(INVALID),
    };
    Ok(Some((tag, value)))
}

pub(crate) fn validate(
    mut model: &[u8],
    input: &[i64],
    output: &[i64],
) -> Result<(), InferenceError> {
    use Field::{Bytes, Integer};

    let mut ir = false;
    let mut opset = false;
    let mut graph = None;
    while let Some(field) = next_field(&mut model)? {
        match field {
            (1, Integer(IR_VERSION)) if !ir => ir = true,
            (2..=4 | 6 | 14, Bytes(_)) | (5, Integer(_)) => {}
            (7, Bytes(value)) if graph.is_none() => graph = Some(value),
            (8, Bytes(value)) if !opset => {
                validate_opset(value)?;
                opset = true;
            }
            _ => return Err(INVALID),
        }
    }
    if !ir || !opset {
        return Err(INVALID);
    }
    validate_graph(graph.ok_or(INVALID)?, input, output)
}

fn validate_opset(mut data: &[u8]) -> Result<(), InferenceError> {
    let mut version = None;
    while let Some(field) = next_field(&mut data)? {
        match field {
            (1, Field::Bytes(b"")) => {}
            (2, Field::Integer(value)) if version.is_none() => version = Some(value),
            _ => return Err(INVALID),
        }
    }
    (version == Some(u64::from(OPSET)))
        .then_some(())
        .ok_or(INVALID)
}

fn validate_graph(mut graph: &[u8], input: &[i64], output: &[i64]) -> Result<(), InferenceError> {
    use Field::Bytes;

    let mut has_in = false;
    let mut has_out = false;
    let mut has_node = false;
    while let Some(field) = next_field(&mut graph)? {
        match field {
            (1, Bytes(node)) => {
                validate_node(node)?;
                has_node = true;
            }
            (5, Bytes(tensor)) => validate_tensor(tensor)?,
            (11, Bytes(value)) if !has_in => {
                validate_value(value, b"images", input)?;
                has_in = true;
            }
            (12, Bytes(value)) if !has_out => {
                validate_value(value, b"output0", output)?;
                has_out = true;
            }
            (2 | 10 | 13 | 16, Bytes(_)) => {}
            _ => return Err(INVALID),
        }
    }
    (has_in && has_out && has_node).then_some(()).ok_or(INVALID)
}

fn validate_node(mut data: &[u8]) -> Result<(), InferenceError> {
    use Field::Bytes;

    let mut operator = None;
    while let Some(field) = next_field(&mut data)? {
        match field {
            (1..=3 | 6 | 9, Bytes(_)) | (7 | 8, Bytes(b"")) => {}
            (4, Bytes(value)) if operator.is_none() => operator = Some(value),
            (5, Bytes(value)) => validate_attribute(value)?,
            _ => return Err(INVALID),
        }
    }
    // Union of the operators in the reviewed square and rectangular exports.
    if matches!(
        operator,
        Some(
            b"Add"
                | b"Cast"
                | b"Concat"
                | b"Constant"
                | b"ConstantOfShape"
                | b"Conv"
                | b"Div"
                | b"Expand"
                | b"Gather"
                | b"MatMul"
                | b"MaxPool"
                | b"Mul"
                | b"Range"
                | b"Reshape"
                | b"Resize"
                | b"Shape"
                | b"Sigmoid"
                | b"Slice"
                | b"Softmax"
                | b"Split"
                | b"Sub"
                | b"Transpose"
                | b"Unsqueeze"
        )
    ) {
        Ok(())
    } else {
        Err(INVALID)
    }
}

fn validate_attribute(mut data: &[u8]) -> Result<(), InferenceError> {
    use Field::{Bytes, Float, Integer};

    while let Some(field) = next_field(&mut data)? {
        match field {
            (1 | 4 | 8 | 13, Bytes(_)) | (2, Float) | (3 | 8, Integer(_)) => {}
            (5, Bytes(tensor)) => validate_tensor(tensor)?,
            (20, Integer(1..=4 | 7)) => {}
            _ => return Err(INVALID),
        }
    }
    Ok(())
}

fn validate_tensor(mut data: &[u8]) -> Result<(), InferenceError> {
    use Field::{Bytes, Integer};

    let mut elements = 1_usize;
    let mut width = None;
    let mut length = None;
    while let Some(field) = next_field(&mut data)? {
        match field {
            (1, Integer(dim)) => elements = count_elements(elements, dim)?,
            (1, Bytes(mut dims)) => {
                while !dims.is_empty() {
                    let dim = decode_varint(&mut dims).map_err(|_| INVALID)?;
                    elements = count_elements(elements, dim)?;
                }
            }
            (2, Integer(dtype @ (1 | 7))) if width.is_none() => {
                width = Some(if dtype == 1 { 4 } else { 8 });
            }
            (8 | 12 | 16, Bytes(_)) | (14, Integer(0)) => {}
            (9, Bytes(raw)) if length.is_none() => length = Some(raw.len()),
            _ => return Err(INVALID),
        }
    }
    let expected = elements.checked_mul(width.ok_or(INVALID)?).ok_or(INVALID)?;
    (length == Some(expected)).then_some(()).ok_or(INVALID)
}

fn count_elements(elements: usize, dim: u64) -> Result<usize, InferenceError> {
    let dim = i64::try_from(dim).map_err(|_| INVALID)?;
    let dim = usize::try_from(dim).map_err(|_| INVALID)?;
    elements.checked_mul(dim).ok_or(INVALID)
}

fn validate_value(mut data: &[u8], name: &[u8], dims: &[i64]) -> Result<(), InferenceError> {
    use Field::{Bytes, Integer};

    let mut named = false;
    let mut tensor = None;
    while let Some(field) = next_field(&mut data)? {
        match field {
            (1, Bytes(value)) if !named && value == name => named = true,
            (2, Bytes(value)) if tensor.is_none() => {
                let mut value = value;
                let Some((1, Bytes(value_tensor))) = next_field(&mut value)? else {
                    return Err(INVALID);
                };
                if !value.is_empty() {
                    return Err(INVALID);
                }
                tensor = Some(value_tensor);
            }
            (3 | 4, Bytes(_)) => {}
            _ => return Err(INVALID),
        }
    }
    if !named {
        return Err(INVALID);
    }
    let mut tensor = tensor.ok_or(INVALID)?;
    let mut float = false;
    let mut shaped = false;
    while let Some(field) = next_field(&mut tensor)? {
        match field {
            (1, Integer(1)) if !float => float = true,
            (2, Bytes(value)) if !shaped => {
                validate_shape(value, dims)?;
                shaped = true;
            }
            _ => return Err(INVALID),
        }
    }
    (float && shaped).then_some(()).ok_or(INVALID)
}

fn validate_shape(mut shape: &[u8], dims: &[i64]) -> Result<(), InferenceError> {
    for &dim in dims {
        let Some((1, Field::Bytes(mut encoded))) = next_field(&mut shape)? else {
            return Err(INVALID);
        };
        match next_field(&mut encoded)? {
            Some((1, Field::Integer(value))) if dim >= 0 && value == dim as u64 => {}
            Some((2, Field::Bytes(symbol)))
                if dim == -1 && !symbol.is_empty() && std::str::from_utf8(symbol).is_ok() => {}
            _ => return Err(INVALID),
        }
        if !encoded.is_empty() {
            return Err(INVALID);
        }
    }
    shape.is_empty().then_some(()).ok_or(INVALID)
}

#[cfg(test)]
mod tests {
    use prost::encoding::{bytes, uint64};

    use super::*;

    fn value(name: &[u8], dims: &[i64], dtype: u64) -> Vec<u8> {
        let mut shape = Vec::new();
        for &dim in dims {
            let mut encoded = Vec::new();
            if dim == -1 {
                bytes::encode(2, &b"dynamic".to_vec(), &mut encoded);
            } else {
                uint64::encode(1, &(dim as u64), &mut encoded);
            }
            bytes::encode(1, &encoded, &mut shape);
        }
        let mut tensor = Vec::new();
        uint64::encode(1, &dtype, &mut tensor);
        bytes::encode(2, &shape, &mut tensor);
        let mut data_type = Vec::new();
        bytes::encode(1, &tensor, &mut data_type);
        let mut value = Vec::new();
        bytes::encode(1, &name.to_vec(), &mut value);
        bytes::encode(2, &data_type, &mut value);
        value
    }

    #[test]
    fn preflight_checks_the_graph_even_when_the_manifest_has_been_verified() {
        let input = [1, 3, -1, -1];
        let output = [1, 6, -1];
        let model = |operator: &[u8], domain: &[u8], external, dtype, opset| {
            let mut node = Vec::new();
            bytes::encode(4, &operator.to_vec(), &mut node);
            bytes::encode(7, &domain.to_vec(), &mut node);
            let mut constant = Vec::new();
            uint64::encode(2, &1, &mut constant);
            bytes::encode(9, &vec![0; 4], &mut constant);
            if external {
                uint64::encode(14, &1, &mut constant);
            }
            let mut attribute = Vec::new();
            bytes::encode(5, &constant, &mut attribute);
            uint64::encode(20, &4, &mut attribute);
            bytes::encode(5, &attribute, &mut node);
            let mut graph = Vec::new();
            bytes::encode(1, &node, &mut graph);
            bytes::encode(11, &value(b"images", &input, dtype), &mut graph);
            bytes::encode(12, &value(b"output0", &output, 1), &mut graph);
            let mut import = Vec::new();
            uint64::encode(2, &opset, &mut import);
            let mut model = Vec::new();
            uint64::encode(1, &IR_VERSION, &mut model);
            bytes::encode(7, &graph, &mut model);
            bytes::encode(8, &import, &mut model);
            model
        };
        let good = model(b"Constant", b"", false, 1, 17);
        assert!(validate(&good, &input, &output).is_ok());
        assert!(validate(&good[..good.len() - 1], &input, &output).is_err());
        assert!(validate(&good, &input, &[1, 56, -1]).is_err());

        for rejected in [
            model(b"Identity", b"", false, 1, 17),
            model(b"Constant", b"custom", false, 1, 17),
            model(b"Constant", b"", true, 1, 17),
            model(b"Constant", b"", false, 7, 17),
            model(b"Constant", b"", false, 1, 18),
        ] {
            assert!(matches!(
                validate(&rejected, &input, &output),
                Err(InferenceError::ModelGraph)
            ));
        }
    }
}
