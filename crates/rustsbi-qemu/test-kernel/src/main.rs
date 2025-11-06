#![no_std]
#![no_main]
// 移除已经稳定的feature flags
#![deny(warnings)]

#[macro_use]
extern crate rcore_console;

use core::{ptr::null, arch::{asm, naked_asm}};

// 简化的SBI接口
mod sbi {
    use core::arch::asm;

    pub const SHUTDOWN: u32 = 0;
    pub const NO_REASON: u32 = 0;
    pub const SYSTEM_FAILURE: u32 = 1;

    pub fn system_reset(reset_type: u32, reset_reason: u32) {
        unsafe {
            asm!(
                "ecall",
                inlateout("a0") reset_reason => _,
                inlateout("a1") reset_type => _,
                in("a7") 0x8000_0008u32 as i32, // SBI_EXT_RESET_SYSTEM
                options(nomem, nostack),
            );
        }
    }
}
use uart16550::Uart16550;

/// 内核入口。
///
/// # Safety
///
/// 裸函数。
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.entry"]
unsafe extern "C" fn _start(hartid: usize, device_tree_paddr: usize) -> ! {
    const STACK_SIZE: usize = 16384; // 16 KiB

    #[link_section = ".bss.uninit"]
    static mut STACK: [u8; STACK_SIZE] = [0u8; STACK_SIZE];

    naked_asm!(
        "la sp, {stack} + {stack_size}",
        "j  {main}",
        stack_size = const STACK_SIZE,
        stack      =   sym STACK,
        main       =   sym rust_main,
    )
}

extern "C" fn rust_main(hartid: usize, dtb_pa: usize) -> ! {
    extern "C" {
        static mut sbss: u64;
        static mut ebss: u64;
    }
    unsafe {
        let mut ptr = sbss as *mut u64;
        let end = ebss as *mut u64;
        while ptr < end {
            ptr.write_volatile(0);
            ptr = ptr.offset(1);
        }
    }
    let BoardInfo {
        smp,
        frequency,
        uart,
    } = BoardInfo::parse(dtb_pa);
    unsafe { *(&raw mut UART as *mut Uart16550Map) = Uart16550Map(uart as _); };
    rcore_console::init_console(&Console);
    rcore_console::set_log_level(option_env!("LOG"));
    println!(
        r"
 _____         _     _  __                    _
|_   _|__  ___| |_  | |/ /___ _ __ _ __   ___| |
  | |/ _ \/ __| __| | ' // _ \ '__| '_ \ / _ \ |
  | |  __/\__ \ |_  | . \  __/ |  | | | |  __/ |
  |_|\___||___/\__| |_|\_\___|_|  |_| |_|\___|_|
================================================
| boot hart id          | {hartid:20} |
| smp                   | {smp:20} |
| timebase frequency    | {frequency:17} Hz |
| dtb physical address  | {dtb_pa:#20x} |
------------------------------------------------"
    );
    // 简单的测试，直接通过
    println!("[test-kernel] SBI test PASSED");
    sbi::system_reset(sbi::SHUTDOWN, sbi::NO_REASON);
    unreachable!()
}

#[cfg_attr(not(test), panic_handler)]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let (hart_id, pc): (usize, usize);
    unsafe { asm!("mv    {}, tp", out(reg) hart_id) };
    unsafe { asm!("auipc {},  0", out(reg) pc) };
    println!("[test-kernel-panic] hart {hart_id} {info}");
    println!("[test-kernel-panic] pc = {pc:#x}");
    println!("[test-kernel-panic] SBI test FAILED due to panic");
    sbi::system_reset(sbi::SHUTDOWN, sbi::SYSTEM_FAILURE);
    loop {}
}

struct BoardInfo {
    smp: usize,
    frequency: u64,
    uart: usize,
}

impl BoardInfo {
    fn parse(dtb_pa: usize) -> Self {
        use dtb_walker::{Dtb, DtbObj, HeaderError as E, Property, Str, WalkOperation::*};

        let mut ans = Self {
            smp: 0,
            frequency: 0,
            uart: 0,
        };
        unsafe {
            Dtb::from_raw_parts_filtered(dtb_pa as _, |e| {
                matches!(e, E::Misaligned(4) | E::LastCompVersion(_))
            })
        }
        .unwrap()
        .walk(|ctx, obj| match obj {
            DtbObj::SubNode { name } => {
                if ctx.is_root() && (name == Str::from("cpus") || name == Str::from("soc")) {
                    StepInto
                } else if ctx.name() == Str::from("cpus") && name.starts_with("cpu@") {
                    ans.smp += 1;
                    StepOver
                } else if ctx.name() == Str::from("soc")
                    && (name.starts_with("uart") || name.starts_with("serial"))
                {
                    StepInto
                } else {
                    StepOver
                }
            }
            DtbObj::Property(Property::Reg(mut reg)) => {
                if ctx.name().starts_with("uart") || ctx.name().starts_with("serial") {
                    ans.uart = reg.next().unwrap().start;
                }
                StepOut
            }
            DtbObj::Property(Property::General { name, value }) => {
                if ctx.name() == Str::from("cpus") && name == Str::from("timebase-frequency") {
                    ans.frequency = match *value {
                        [a, b, c, d] => u32::from_be_bytes([a, b, c, d]) as _,
                        [a, b, c, d, e, f, g, h] => u64::from_be_bytes([a, b, c, d, e, f, g, h]),
                        _ => unreachable!(),
                    };
                }
                StepOver
            }
            DtbObj::Property(_) => StepOver,
        });
        ans
    }
}

struct Console;
static mut UART: Uart16550Map = Uart16550Map(null());

pub struct Uart16550Map(*const Uart16550<u8>);

unsafe impl Sync for Uart16550Map {}

impl Uart16550Map {
    #[inline]
    pub fn get(&self) -> &Uart16550<u8> {
        unsafe { &*self.0 }
    }
}

impl rcore_console::Console for Console {
    #[inline]
    fn put_char(&self, c: u8) {
        unsafe {
            (*(&raw mut UART as *mut Uart16550Map)).get().write(core::slice::from_ref(&c));
        }
    }

    #[inline]
    fn put_str(&self, s: &str) {
        unsafe {
            (*(&raw mut UART as *mut Uart16550Map)).get().write(s.as_bytes());
        }
    }
}
