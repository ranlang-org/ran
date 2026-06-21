//! Hardware & Kernel module - Low-level access for embedded systems.
//! Provides GPIO, memory-mapped I/O, serial, and syscall interfaces.

/// GPIO Pin mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinMode {
    Input,
    Output,
    PullUp,
    PullDown,
}

/// GPIO Pin state
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinState {
    High,
    Low,
}

/// GPIO Pin abstraction
pub struct GpioPin {
    pub number: u32,
    pub mode: PinMode,
    state: PinState,
}

impl GpioPin {
    pub fn new(number: u32, mode: PinMode) -> Self {
        Self {
            number,
            mode,
            state: PinState::Low,
        }
    }

    pub fn write(&mut self, state: PinState) {
        self.state = state;
    }

    pub fn read(&self) -> PinState {
        self.state
    }

    pub fn toggle(&mut self) {
        self.state = match self.state {
            PinState::High => PinState::Low,
            PinState::Low => PinState::High,
        };
    }
}

/// Memory-mapped I/O register access (unsafe by design)
pub struct MmioRegister {
    base_address: usize,
}

impl MmioRegister {
    pub fn new(base: usize) -> Self {
        Self { base_address: base }
    }

    /// Read a 32-bit value from offset
    ///
    /// # Safety
    /// Caller must ensure the address is valid and mapped.
    pub unsafe fn read32(&self, offset: usize) -> u32 {
        let ptr = (self.base_address + offset) as *const u32;
        core::ptr::read_volatile(ptr)
    }

    /// Write a 32-bit value to offset
    ///
    /// # Safety
    /// Caller must ensure the address is valid and mapped.
    pub unsafe fn write32(&self, offset: usize, value: u32) {
        let ptr = (self.base_address + offset) as *mut u32;
        core::ptr::write_volatile(ptr, value);
    }
}

/// Serial/UART communication
pub struct Serial {
    port_path: String,
    baud_rate: u32,
}

impl Serial {
    pub fn new(port: &str, baud: u32) -> Self {
        Self {
            port_path: port.to_string(),
            baud_rate: baud,
        }
    }

    pub fn port(&self) -> &str {
        &self.port_path
    }

    pub fn baud(&self) -> u32 {
        self.baud_rate
    }
}

/// System call wrapper (Linux x86_64)
pub mod syscall {
    /// Execute a raw syscall
    ///
    /// # Safety
    /// Caller is responsible for passing valid syscall numbers and arguments.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub unsafe fn raw(num: i64, arg1: i64, arg2: i64, arg3: i64) -> i64 {
        let ret: i64;
        core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
        );
        ret
    }

    /// Fallback for non-Linux platforms
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    pub unsafe fn raw(_num: i64, _arg1: i64, _arg2: i64, _arg3: i64) -> i64 {
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpio_pin() {
        let mut pin = GpioPin::new(13, PinMode::Output);
        assert_eq!(pin.read(), PinState::Low);
        pin.write(PinState::High);
        assert_eq!(pin.read(), PinState::High);
        pin.toggle();
        assert_eq!(pin.read(), PinState::Low);
    }
}
