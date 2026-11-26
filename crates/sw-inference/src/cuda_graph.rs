use ort::{
    memory::{AllocationDevice, Allocator, AllocatorType, MemoryInfo, MemoryType},
    session::{IoBinding, Session},
    value::{Shape, Tensor, TensorRef},
};

use crate::{InferenceError, TensorSize, ort_error};

/// One capture per active model. The bound addresses remain fixed until the
/// worker exits, and its serial request loop owns both transfer buffers.
pub(crate) struct CudaGraph {
    binding: IoBinding,
    device_input: Tensor<f32>,
    device_output: Tensor<f32>,
    host_output: Tensor<f32>,
    size: TensorSize,
}

impl CudaGraph {
    pub(crate) fn new(
        session: &Session,
        device: u16,
        size: TensorSize,
        channels: usize,
    ) -> Result<Self, InferenceError> {
        let memory = MemoryInfo::new(
            AllocationDevice::CUDA,
            i32::from(device),
            AllocatorType::Device,
            MemoryType::Default,
        )
        .map_err(ort_error)?;
        let allocator = Allocator::new(session, memory).map_err(ort_error)?;
        let [height, width] = size.dimensions();
        let output_shape = [1, channels, size.anchors()];
        let device_input = Tensor::new(&allocator, [1, 3, height, width]).map_err(ort_error)?;
        let mut device_output = Tensor::new(&allocator, output_shape).map_err(ort_error)?;
        let host_output = Tensor::new(&Allocator::default(), output_shape).map_err(ort_error)?;
        let mut binding = session.create_binding().map_err(ort_error)?;
        binding
            .bind_input("images", &device_input)
            .map_err(ort_error)?;
        binding
            .bind_output("output0", &mut device_output)
            .map_err(ort_error)?;
        Ok(Self {
            binding,
            device_input,
            device_output,
            host_output,
            size,
        })
    }

    pub(crate) fn run(
        &mut self,
        session: &mut Session,
        size: TensorSize,
        input: TensorRef<'_, f32>,
    ) -> Result<(&Shape, &[f32]), InferenceError> {
        if size != self.size {
            return Err(InferenceError::Tensor);
        }
        input.copy_into(&mut self.device_input).map_err(ort_error)?;
        drop(session.run_binding(&self.binding).map_err(ort_error)?);
        self.device_output
            .copy_into(&mut self.host_output)
            .map_err(ort_error)?;
        self.host_output
            .try_extract_tensor::<f32>()
            .map_err(ort_error)
    }
}
