#![allow(dead_code)]

use log::*;
use anyhow::*;
use std::mem;
use std::ptr;
use std::slice;

mod findpattern;
mod global_handle;
mod pointer;

pub mod handle_interfaces;
pub mod scan;

pub use findpattern::*;
pub use global_handle::*;
pub use pointer::*;
pub use scan::*;


use std::borrow::Borrow;
use crate::memory::handle_interfaces::driver_handle::DriverProcessHandle;

/// Defines the game address width based on if the `32-bit` feature is set
#[cfg(feature = "32-bit")]
pub type Address = u32;
#[cfg(not(feature = "32-bit"))]
pub type Address = u64;

/// An abstract interface for reading and writing memory to a process
/// allowing cross platform interaction with a process.
/// This is what the ProcessHandle is built off of
pub trait ProcessHandleInterface {
    /// Reads `size` bytes from a at the specified `address`.
    /// If it is successful, it will return a boxed byte slice
    /// Otherwise, it will return the error.
    fn read_bytes(&self, address: Address, size: usize) -> Result<Box<[u8]>>;

    /// Write a slice of bytes to a process at the address `address`
    /// Returns an error if unsuccessful
    fn write_bytes(&self, address: Address, bytes: &[u8]) -> Result<()>;

    /// Gets information about a module in the form of a Module struct by name
    /// If the module is found, it will return Some with the Module object,
    /// Otherwise, it will return None
    fn get_module(&self, module_name: &str) -> Option<Module>;

    /// Returns a struct of process info useful in some cheats
    fn get_process_info(&self) -> ProcessInfo;
}

pub struct ProcessInfo {
    /// The base address of the PEB. Needed in some games
    pub peb_base_address: u64,
    /// The name of the process
    pub process_name: String,
    /// 32 bit or 64 bit (number is either 32 or 64)
    pub bitness: u16,
    /// Pid of process
    pub pid: u32
}

/// A handle to a process allowing Reading and writing memory
pub struct Handle {
    interface: Box<dyn ProcessHandleInterface>,
}

impl<T: 'static + ProcessHandleInterface> From<T> for Handle {
    fn from(interface: T) -> Self {
        Self::from_boxed_interface(Box::new(interface))
    }
}

impl Handle {
    /// Creates a new Handle using the intrinsic process handle interface and the process name
    pub fn from_boxed_interface(interface: Box<dyn ProcessHandleInterface>) -> Self {
        Handle { interface }
    }

    pub fn from_interface<T: 'static + ProcessHandleInterface>(interface: T) -> Self {
        let interface = Box::new(interface);
        Self { interface }
    }

    #[cfg(target_os = "linux")]
    /// Automatically finds the most secure method of reading / writing
    /// memory and creates a process handle using it
    ///
    /// For example, if the program was running on linux
    /// with a KVM, a KVM handle would be created
    pub fn new(process_name: impl ToString) -> Result<Handle> {
        let process_name = process_name.to_string();
        Ok(Self::from_boxed_interface(kvm_handle::KVMProcessHandle::attach(
            &process_name,
        )?))
    }

    #[cfg(target_os = "windows")]
    /// Automatically finds the most secure method of reading / writing
    /// memory and creates a process handle using it
    ///
    /// For example, if the program was running on linux
    /// with a KVM, a KVM handle would be created
    pub fn new(process_name: impl ToString) -> Result<Handle> {
        let process_name = process_name.to_string();
        info!("Creating a process handle to {}", process_name);
        Ok(Self::from_interface(DriverProcessHandle::attach(process_name)?))
    }

    /// Reads memory of type T from a process. If it is successful,
    /// it will return the bytes read as type T. Otherwise, the error will be returned
    pub fn try_read_memory<T>(&self, address: Address) -> Result<T> {
        // Get size of the type
        let size = mem::size_of::<T>();

        let bytes = self
            .interface
            .read_bytes(address, size).context(anyhow!("Could not read 0x{:X}", address))?;
        // Convert the raw bytes into the type we need to return
        let value = unsafe {
            // We do this by casting the pointer to the bytes as a pointer to T
            ptr::read(bytes.as_ptr() as *const _)
        };

        Ok(value)
    }

    /// Reads memory of type T from a process. If it is successful,
    /// it will return the bytes read as type T. Otherwise, it will panic.
    pub fn read_memory<T>(&self, address: Address) -> T {
        self.try_read_memory(address).unwrap()
    }

    /// Writes memory of type T to a process. Returns Ok(()) if successful
    pub fn try_write_memory<T>(&self, address: Address, value: T) -> Result<()> {
        let size = mem::size_of::<T>();

        // Create a byte buffer from the type
        // https://stackoverflow.com/a/42186553
        let buff = unsafe { slice::from_raw_parts((&value as *const T) as *const u8, size) };

        self.write_bytes(address, buff).context(anyhow!("Could not write 0x{:X}", address))
    }

    /// Writes memory of type T to a process. If it is successful,
    /// the function will return, otherwise the function will panic
    pub fn write_memory<T>(&self, address: Address, value: T) {
        self.try_write_memory(address, value).unwrap()
    }

    /// Reads an array of length `length` and type T from the process.
    /// If successful, it will return the read array as a Vec<T>,
    /// Otherwise, the function will panic
    pub fn read_array<T>(&self, address: Address, length: usize) -> Vec<T> {
        let size = std::mem::size_of::<T>() as u32;

        // Creates an array lf values for our result
        let mut values = Vec::new();

        // Read memory at each address
        for i in 0..length {
            // Multiply index by size to get the pointer for the index
            let address = address + (i * size as usize) as Address;
            values.push(self.read_memory(address));
        }

        // Return the values
        values
    }

    /// Dumps memory from memory_range.0 to memory_range.1
    /// Returns a boxed byte slice
    pub fn dump_memory(&self, memory_range: (Address, Address)) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::new();

        trace!(
            "Dumping {} bytes of memory starting at 0x{:X}",
            memory_range.1 - memory_range.0,
            memory_range.0
        );

        // The amount of bytes to be read at a time
        let chunk_size: usize = 4096;

        // The current memory location we are reading
        let mut current_offset: Address = memory_range.0;

        loop {
            // The current offset should never be greater than the module_end_address
            if current_offset > memory_range.1 {
                dbg!(current_offset, memory_range.1);
                unreachable!("dump_module attempted to read invalid memory")
            }
            if current_offset == memory_range.1 {
                break;
            }

            // Create the size based on the current offset
            let read_size = {
                // If we would read memory which is out of bounds, resize the read_size accordingly
                if current_offset + chunk_size as Address > memory_range.1 {
                    (memory_range.1 - current_offset) as usize
                } else {
                    chunk_size
                }
            };

            let memory = self.read_bytes(current_offset, read_size).unwrap_or_else(|e| {
                debug!("Could not read {} bytes at 0x{:X}, defaulting to 0", read_size, current_offset);
                vec![0; read_size].into_boxed_slice()
            });

            // Append the slice of memory to the buffer
            buffer.extend_from_slice(&memory);

            current_offset += read_size as Address;
        }

        buffer
    }

    /// Reads a null terminated i8 array string starting at `address`
    /// If the string is longer than 4096 characters, it will only read
    /// the first 4096 characters
    pub fn read_string(&self, address: Address) -> String {
        let mut bytes: Vec<u8> = Vec::new();

        for i in 0..4096 {
            // Read the byte at index i from memory
            let byte: i8 = self.read_memory(address + i);

            // If the byte is a null terminator, break
            if byte == 0 {
                break;
            }

            // Convert i8 to u8 in the `bytes` vec
            bytes.push(byte as _)
        }

        String::from_utf8(bytes).unwrap()
    }

    // -------------------------------------------------------- //
    // Implement the intrinsic `ProcessHandleInterface` methods //
    // -------------------------------------------------------- //

    /// Reads `size` bytes from a at the specified `address`.
    /// If it is successful, it will return a boxed byte slice
    /// Otherwise, it will return the error.
    pub fn read_bytes(&self, address: Address, size: usize) -> Result<Box<[u8]>> {
        trace!("Reading {} bytes of memory at 0x{:X}", size, address);
        self.interface.read_bytes(address, size)
    }

    /// Write a slice of bytes to a process at the address `address`
    /// Returns an error if unsuccessful
    pub fn write_bytes(&self, address: Address, bytes: &[u8]) -> Result<()> {
        trace!("Writing {} bytes of memory at 0x{:X}", bytes.len(), address);
        self.interface.write_bytes(address, bytes)
    }

    /// Gets information about a module in the form of a Module struct by name
    /// If the module is found, it will return Some with the Module object,
    /// Otherwise, it will return None
    pub fn get_module(&self, module_name: impl ToString) -> Option<Module> {
        let module_name = module_name.to_string();
        let module = self.interface.get_module(&module_name);
        if module.is_some() {
            let module = module.borrow().as_ref().unwrap();
            debug!(
                "Found module {} with base address 0x{:X}",
                module.name, module.base_address
            )
        }
        module
    }

    /// Returns a struct of process info useful in some cheats
    pub fn get_process_info(&self) -> ProcessInfo {
        self.interface.get_process_info()
    }
}

/// Defines information about a module
#[derive(Clone, Debug)]
pub struct Module {
    /// The image base address
    pub base_address: Address,
    /// Size in bytes of the module
    pub size: u64,
    /// The name of the module
    pub name: String,
}

impl Module {
    /// Returns the range of memory for the entire module
    pub fn get_memory_range(&self) -> (Address, Address) {
        (self.base_address, self.base_address + self.size as Address)
    }
}
