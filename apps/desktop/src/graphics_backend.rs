//! Surface-aware X11 graphics selection. Capture and GIF pixels are independent.

use std::sync::Arc;

use eframe::egui_wgpu::WgpuSetup;
use gif_from_screen_capture_linux::LinuxDisplayServer;
use wgpu::{Adapter, Backend, Backends, DeviceType, PowerPreference, Surface};

pub(crate) fn configure(
    options: &mut eframe::NativeOptions,
    display: Option<LinuxDisplayServer>,
    requested: Option<Backends>,
) {
    let WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup else {
        return;
    };
    setup.native_adapter_selector = None;
    if let Some(backends) = requested {
        // An explicit diagnostic override is a restriction, not a preference.
        setup.instance_descriptor.backends = backends;
        return;
    }
    if display != Some(LinuxDisplayServer::X11) {
        return;
    }

    // Keep the verified GL transparent-window path first, but never require it.
    // In particular, NVIDIA may expose GL adapters incompatible with X11 ARGB
    // surfaces while its Vulkan adapter can present to the very same window.
    setup.instance_descriptor.backends = Backends::GL | Backends::VULKAN;
    let power_preference = setup.power_preference;
    let device_descriptor = Arc::clone(&setup.device_descriptor);
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        let candidates: Vec<_> = adapters
            .iter()
            .map(|adapter| {
                let information = adapter.get_info();
                let required = device_descriptor(adapter);
                Candidate {
                    backend: information.backend,
                    device_type: information.device_type,
                    usable: supports_surface(adapter, surface)
                        && adapter.features().contains(required.required_features)
                        && required.required_limits.check_limits(&adapter.limits()),
                }
            })
            .collect();
        let index = select_candidate(&candidates, power_preference).ok_or_else(|| {
            "No compatible graphics adapter can render this X11 window. Checked OpenGL and Vulkan, \
             including installed software adapters. Check the graphics driver and display session. \
             WGPU_BACKEND=gl or WGPU_BACKEND=vulkan can be used for diagnosis."
                .to_owned()
        })?;
        let information = adapters[index].get_info();
        eprintln!(
            "GifFromScreen graphics: {:?} ({})",
            information.backend, information.name
        );
        Ok(adapters[index].clone())
    }));
}

fn supports_surface(adapter: &Adapter, surface: Option<&Surface<'_>>) -> bool {
    surface.is_none_or(|surface| {
        if !adapter.is_surface_supported(surface) {
            return false;
        }
        let capabilities = surface.get_capabilities(adapter);
        !capabilities.formats.is_empty()
            && !capabilities.present_modes.is_empty()
            && capabilities
                .usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
    })
}

#[derive(Clone, Copy)]
struct Candidate {
    backend: Backend,
    device_type: DeviceType,
    usable: bool,
}

fn select_candidate(candidates: &[Candidate], power: PowerPreference) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.usable && matches!(candidate.backend, Backend::Gl | Backend::Vulkan)
        })
        .min_by_key(|(_, candidate)| {
            (
                u8::from(candidate.backend != Backend::Gl),
                device_rank(candidate.device_type, power),
            )
        })
        .map(|(index, _)| index)
}

fn device_rank(device: DeviceType, power: PowerPreference) -> u8 {
    match (device, power) {
        (DeviceType::IntegratedGpu, PowerPreference::LowPower)
        | (DeviceType::DiscreteGpu, PowerPreference::HighPerformance) => 0,
        (DeviceType::IntegratedGpu | DeviceType::DiscreteGpu, _) => 1,
        (DeviceType::VirtualGpu, _) => 2,
        (DeviceType::Other, _) => 3,
        (DeviceType::Cpu, _) => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(backend: Backend, device_type: DeviceType, usable: bool) -> Candidate {
        Candidate {
            backend,
            device_type,
            usable,
        }
    }

    #[test]
    fn incompatible_gl_falls_back_to_vulkan() {
        let candidates = [
            candidate(Backend::Gl, DeviceType::DiscreteGpu, false),
            candidate(Backend::Vulkan, DeviceType::DiscreteGpu, true),
        ];
        assert_eq!(
            select_candidate(&candidates, PowerPreference::HighPerformance),
            Some(1)
        );
    }

    #[test]
    fn compatible_gl_stays_preferred_over_software_vulkan() {
        let candidates = [
            candidate(Backend::Vulkan, DeviceType::Cpu, true),
            candidate(Backend::Gl, DeviceType::Cpu, true),
        ];
        assert_eq!(
            select_candidate(&candidates, PowerPreference::HighPerformance),
            Some(1)
        );
    }

    #[test]
    fn incompatible_devices_are_never_selected_even_if_they_have_higher_priority() {
        let candidates = [
            candidate(Backend::Gl, DeviceType::DiscreteGpu, false),
            candidate(Backend::Vulkan, DeviceType::DiscreteGpu, false),
            candidate(Backend::Vulkan, DeviceType::Cpu, true),
        ];
        assert_eq!(
            select_candidate(&candidates, PowerPreference::HighPerformance),
            Some(2)
        );
    }

    #[test]
    fn power_preference_orders_compatible_devices_within_a_backend() {
        let candidates = [
            candidate(Backend::Vulkan, DeviceType::Cpu, true),
            candidate(Backend::Vulkan, DeviceType::IntegratedGpu, true),
            candidate(Backend::Vulkan, DeviceType::DiscreteGpu, true),
        ];
        assert_eq!(
            select_candidate(&candidates, PowerPreference::LowPower),
            Some(1)
        );
        assert_eq!(
            select_candidate(&candidates, PowerPreference::HighPerformance),
            Some(2)
        );
        assert_eq!(
            select_candidate(&candidates, PowerPreference::None),
            Some(1)
        );
    }

    #[test]
    fn no_compatible_adapter_returns_none_without_guessing() {
        assert_eq!(select_candidate(&[], PowerPreference::None), None);
        let candidates = [
            candidate(Backend::Gl, DeviceType::DiscreteGpu, false),
            candidate(Backend::Vulkan, DeviceType::Cpu, false),
            candidate(Backend::Metal, DeviceType::DiscreteGpu, true),
        ];
        assert_eq!(select_candidate(&candidates, PowerPreference::None), None);
    }

    #[test]
    fn automatic_x11_enables_both_backends_and_installs_a_surface_selector() {
        let mut options = eframe::NativeOptions::default();
        configure(&mut options, Some(LinuxDisplayServer::X11), None);
        let WgpuSetup::CreateNew(setup) = &options.wgpu_options.wgpu_setup else {
            panic!("expected a new device");
        };
        assert_eq!(
            setup.instance_descriptor.backends,
            Backends::GL | Backends::VULKAN
        );
        assert!(setup.native_adapter_selector.is_some());
        let error = setup.native_adapter_selector.as_ref().unwrap()(&[], None).unwrap_err();
        assert!(error.contains("OpenGL and Vulkan"));
    }

    #[test]
    fn explicit_backend_overrides_remove_automatic_selection() {
        for backends in [
            Backends::GL,
            Backends::VULKAN,
            Backends::GL | Backends::VULKAN,
            Backends::empty(),
        ] {
            let mut options = eframe::NativeOptions::default();
            configure(&mut options, Some(LinuxDisplayServer::X11), None);
            configure(&mut options, Some(LinuxDisplayServer::X11), Some(backends));
            let WgpuSetup::CreateNew(setup) = &options.wgpu_options.wgpu_setup else {
                panic!("expected a new device");
            };
            assert_eq!(setup.instance_descriptor.backends, backends);
            assert!(setup.native_adapter_selector.is_none());
        }
    }

    #[test]
    fn wayland_keeps_the_framework_default_selection() {
        let mut options = eframe::NativeOptions::default();
        let WgpuSetup::CreateNew(before) = &options.wgpu_options.wgpu_setup else {
            panic!("expected a new device");
        };
        let backends = before.instance_descriptor.backends;
        configure(&mut options, Some(LinuxDisplayServer::Wayland), None);
        let WgpuSetup::CreateNew(after) = &options.wgpu_options.wgpu_setup else {
            panic!("expected a new device");
        };
        assert_eq!(after.instance_descriptor.backends, backends);
        assert!(after.native_adapter_selector.is_none());
    }
}
