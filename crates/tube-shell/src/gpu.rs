//! Adapter selection and the headless `--self-check` probe.

/// Vulkan is the primary backend (FIRST-SLICE.md). `WGPU_BACKEND` overrides it
/// for debugging on other stacks; there is no WebGL2 path anywhere.
pub fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN);
    wgpu::Instance::new(descriptor)
}

/// Request an adapter and a device with no window and no surface, and report
/// what turned up.
///
/// `vulkaninfo` is not installed on the target machine, so this is the
/// mechanical probe of whether the Vulkan runtime is actually up.
pub fn self_check() -> Result<(), String> {
    let instance = instance();

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        ..Default::default()
    }))
    .map_err(|e| {
        format!(
            "no Vulkan adapter: {e}\n\
             The renderer needs a working Vulkan stack (mesa-vulkan-drivers or the \
             vendor driver, plus vulkan-loader). Set WGPU_BACKEND to try another \
             backend."
        )
    })?;

    let info = adapter.get_info();
    println!(
        "adapter: {} ({:?}, {:?})\ndriver:  {} {}",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    );

    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("self-check"),
        ..Default::default()
    }))
    .map_err(|e| format!("adapter found but no device: {e}"))?;

    println!("device:  ok");
    Ok(())
}
