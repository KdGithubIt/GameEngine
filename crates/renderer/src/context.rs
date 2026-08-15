use std::fmt;
use wgpu::{Adapter, Device, Instance, Queue, Surface};

/// Configures GPU adapter selection and logical device creation.
pub struct GpuContextDescriptor<'a> {
    /// The preferred adapter power profile.
    pub power_preference: wgpu::PowerPreference,
    /// Whether adapter selection must use a fallback software adapter.
    pub force_fallback_adapter: bool,
    /// The features, limits, and label requested from the selected adapter.
    pub device: wgpu::DeviceDescriptor<'a>,
}

impl Default for GpuContextDescriptor<'_> {
    fn default() -> Self {
        Self {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            device: wgpu::DeviceDescriptor::default(),
        }
    }
}

/// Reports a failure while selecting an adapter or creating a logical device.
#[derive(Debug)]
pub enum GpuContextError {
    /// No adapter compatible with the requested surface was available.
    AdapterUnavailable,
    /// The selected adapter could not create the requested device.
    RequestDevice(wgpu::RequestDeviceError),
}

impl fmt::Display for GpuContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdapterUnavailable => formatter.write_str("no compatible GPU adapter was found"),
            Self::RequestDevice(error) => {
                write!(formatter, "failed to request GPU device: {error}")
            }
        }
    }
}

impl std::error::Error for GpuContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AdapterUnavailable => None,
            Self::RequestDevice(error) => Some(error),
        }
    }
}

/// Owns a selected GPU adapter, logical device, and submission queue.
pub struct GpuContext {
    adapter: Adapter,
    device: Device,
    queue: Queue,
}

impl GpuContext {
    /// Selects an adapter and requests its default logical device.
    ///
    /// # Errors
    ///
    /// Returns an error when no compatible adapter is available or the adapter
    /// cannot create the requested device.
    pub async fn new(
        instance: &Instance,
        compatible_surface: Option<&Surface<'_>>,
    ) -> Result<Self, GpuContextError> {
        let descriptor = GpuContextDescriptor::default();
        Self::new_with_descriptor(instance, compatible_surface, &descriptor).await
    }

    /// Selects an adapter and requests a logical device using `descriptor`.
    ///
    /// # Errors
    ///
    /// Returns an error when no compatible adapter is available or the adapter
    /// cannot satisfy the requested device descriptor.
    pub async fn new_with_descriptor(
        instance: &Instance,
        compatible_surface: Option<&Surface<'_>>,
        descriptor: &GpuContextDescriptor<'_>,
    ) -> Result<Self, GpuContextError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: descriptor.power_preference,
                compatible_surface,
                force_fallback_adapter: descriptor.force_fallback_adapter,
            })
            .await
            .map_err(|_| GpuContextError::AdapterUnavailable)?;
        let (device, queue) = adapter
            .request_device(&descriptor.device)
            .await
            .map_err(GpuContextError::RequestDevice)?;

        Ok(Self {
            adapter,
            device,
            queue,
        })
    }

    /// Returns the selected physical or virtual GPU adapter.
    pub fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// Returns the logical GPU device used to create rendering resources.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Returns the queue used to submit commands and upload data.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }
}
