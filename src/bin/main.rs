#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::main;
use esp_hal::rmt::Rmt;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_wifi::wifi::WifiStaDevice;
use ieee80211::{data_frame::DataFrame, match_frames};

use critical_section::with;
use ieee80211::macro_bits;
use log::info;

// LED Control
use fugit::Rate;
use smart_leds::{brightness, gamma, Brightness, SmartLedsWrite};

mod esp_hal_smartled;
use crate::esp_hal_smartled::SmartLedsAdapter;

use smart_leds::{hsv::hsv2rgb, hsv::Hsv, RGB8};

extern crate alloc;

// Const Configuration
const ROW_SIZE: usize = 8;
const COL_SIZE: usize = 32;
const BRIGHTNESS: u8 = 128;
const LED_COUNT: usize = ROW_SIZE * COL_SIZE;

// Dynamic Configuration Unsafe
static mut cooling: u32 = 10;
static mut frame_rate: u32 = 30;
static mut sparking: u32 = 128;

// Packet Counter
static mut total_packet: u32 = 0;
static mut total_byte: u32 = 0;

// rng
static mut rng_global: Option<Rng> = None;
static mut heat: [u8; LED_COUNT] = [0; LED_COUNT];

fn map_xy_to_index(x: usize, y: usize) -> usize {
    y * ROW_SIZE + x
}

fn map_range(value: u32, from_low: u32, from_high: u32, to_low: u32, to_high: u32) -> u32 {
    (value - from_low) * (to_high - to_low) / (from_high - from_low) + to_low
}

fn get_random(x: u32, y: u32) -> u8 {
    unsafe {
        match rng_global {
            Some(mut rng) => {
                let random_value = rng.random() % (y - x);
                return (x + random_value) as u8;
            }
            None => {
                return 0;
            }
        }
    }
}

fn fire_sim_on_line(data: &mut [RGB8]) {
    unsafe {
        for y in 0..COL_SIZE {
            // cooling
            for x in 0..ROW_SIZE {
                let index = map_xy_to_index(x, y);
                let cooling_coef = get_random(0, cooling * 10 / ROW_SIZE as u32 + 2) as u8;
                heat[index] = heat[index].saturating_sub(cooling_coef);
            }

            // Spreading
            for x in 1..(ROW_SIZE - 1) {
                let index = map_xy_to_index(x, y);

                heat[index] = heat[map_xy_to_index(x, y)]
                    .saturating_add(heat[map_xy_to_index(x - 1, y)])
                    .saturating_add(heat[map_xy_to_index(x + 1, y)])
                    / 3;
            }

            if get_random(0, 255) < sparking as u8 && total_byte > 0 {
                let x = get_random(0, ROW_SIZE as u32) as usize;

                let index = map_xy_to_index(x, y);

                heat[index] = heat[index].saturating_add(255);

                total_byte.saturating_sub(128);
            }
        }

        // Map from heat to LED colors
        for x in 0..LED_COUNT {
            let brightness = heat[x] as u8;
            let mut sat = 255;
            if brightness > 240 {
                sat = (brightness - 240);
            }

            let color = Hsv {
                hue: map_range(255 - brightness as u32, 0, 255, 80, 100) as u8,
                sat: sat,
                val: brightness,
            };

            data[x] = hsv2rgb(color);
        }
    }
}

#[main]
fn main() -> ! {
    // generator version: 0.2.2

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_println::logger::init_logger_from_env();

    esp_alloc::heap_allocator!(72 * 1024);

    // Initialize RNG
    let rng = Rng::new(peripherals.RNG);
    unsafe {
        rng_global = Some(rng);
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let init = esp_wifi::init(timg0.timer0, rng, peripherals.RADIO_CLK).unwrap();

    // Initialize Wi-Fi in station mode
    let (mut _device, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, peripherals.WIFI, WifiStaDevice).unwrap();

    // Start Wi-Fi
    controller.start().unwrap();

    // Get sniffer and enable promiscuous mode
    let mut sniffer = controller.take_sniffer().unwrap();
    sniffer.set_promiscuous_mode(true).unwrap();

    // Enable packet logging
    sniffer.set_receive_cb(|packet| {
        let _ = match_frames! {
            packet.data,
            _data = DataFrame => {
                unsafe {
                    total_packet += 1;
                    total_byte += packet.len as u32;
                }
            }
        };
    });

    let delay = Delay::new();

    // Set up LED Driver
    let rmt = Rmt::new(peripherals.RMT, Rate::<u32, 1, 1>::from_raw(80000000)).unwrap();
    let rmt_buffer = smartLedBuffer!(LED_COUNT);

    let mut led = SmartLedsAdapter::new(rmt.channel0, peripherals.GPIO13, rmt_buffer);

    let mut data = [RGB8::default(); LED_COUNT];

    let mut seconds_counter = 0;

    loop {
        fire_sim_on_line(&mut data);
        let brightness_adjust = brightness(data.iter().cloned(), 200);
        let gamma_adjust = gamma(brightness_adjust);

        with(|cs| {
            led.write(gamma_adjust).unwrap();
        });

        unsafe {
            if seconds_counter > 1000 {
                info!("Total Packet: {}, Total Byte: {}", total_packet, total_byte);

                cooling = map_range(total_packet, 0, 50, 0, 100);
                sparking = map_range(total_byte, 0, 5000, 0, 255);
                total_byte = 0;
                total_packet = 0;
                seconds_counter = 0;
            }

            seconds_counter += frame_rate;
            delay.delay_millis(1000 / frame_rate);
        }
    }
}
