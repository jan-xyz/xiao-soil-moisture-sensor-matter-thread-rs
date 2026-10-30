//! The composition root: the only place that knows every layer.
//!
//! It builds the peripherals, wires them into the decision layer through the
//! device layer's ports, runs one boot, and performs whatever that boot asked
//! for. It holds no policy of its own - see `ARCHITECTURE.md`, and the tuning
//! module in the decision layer, for where the numbers live.
//!
//! The task graph is small on purpose: the protocol stack with its writer of
//! the CASE resumption records, the boot loop, and the LED task that must not
//! miss a command while the boot loop is busy. The button runs from the very
//! start of the boot instead, because its press is what may have woken the
//! device.

#![no_std]
#![no_main]
#![recursion_limit = "256"]

use core::pin::pin;

use core::cell::RefCell;
use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::Channel;

use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_bootloader_esp_idf::partitions::{
    read_partition_table, DataPartitionSubType, PartitionType, PARTITION_TABLE_MAX_LEN,
};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::gpio::{Flex, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::ram;
use esp_hal::rtc_cntl::sleep::{LowPower, RtcSleepConfig};
use esp_hal::rtc_cntl::{wakeup_cause, Rtc, RwdtStage, SocResetReason, WakeupSource};
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use esp_storage::FlashStorage;

use log::{info, warn};

use rs_matter_embassy::matter::crypto::{default_crypto, Crypto, Rng};
use rs_matter_embassy::matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter_embassy::matter::dm::clusters::decl::soil_measurement::ClusterHandler as _;
use rs_matter_embassy::matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter_embassy::matter::dm::clusters::gen_diag::BootReasonEnum;
use rs_matter_embassy::matter::dm::clusters::icd_mgmt::{
    ClusterHandler as _, IcdModeConfig, LitIcd, LitIcdMgmtHandler,
};
use rs_matter_embassy::matter::dm::devices::test::{
    DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET,
};
use rs_matter_embassy::matter::dm::devices::DEV_TYPE_ROOT_NODE;
use rs_matter_embassy::matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter_embassy::matter::dm::{Async, Cluster, Dataver, EmptyHandler, Endpoint, Node};
use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
use rs_matter_embassy::matter::{clusters, devices, BasicCommData};
use rs_matter_embassy::persist::SeqMapKvBlobStore;
use rs_matter_embassy::stack::rand::rand_core::SeedableRng;
use rs_matter_embassy::stack::rand::ChaCha12Rng;
use rs_matter_embassy::wireless::esp::EspThreadDriver;
use rs_matter_embassy::wireless::{EmbassyThread, EmbassyThreadMatterStack};

use soil_sensor_core::app::{run_boot, run_wake, Boot};
use soil_sensor_core::ports::{ButtonEdge, Outcome, Ports as _, WakeCause};
use soil_sensor_core::recovery::recover;
use soil_sensor_core::tuning;
use soil_sensor_node::{
    BatteryCell, PowerSourceHandler, SoilMeasurementHandler, SoilMoistureCell, DEV_TYPE_SOIL_SENSOR,
};

use tinyrlibc as _;

extern crate alloc;

mod battery;
mod board;
mod button;
mod flash;
mod leds;
mod ports;
mod probe;
mod store;

use battery::Battery;
use flash::{FlashCell, SharedFlash, SharedFlashBus};
use leds::StatusLeds;
use ports::EspPorts;
use probe::SoilProbe;

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit()
    }};
}

/// See the reference example for the rationale on this sizing; retuned upward
/// because this device hosts two hand-written cluster handlers and the boot loop.
const BUMP_SIZE: usize = 24000;

/// Heap for Thread+BLE plus the boot loop's own allocations.
const HEAP_SIZE: usize = 110 * 1024;

const RECLAIMED_RAM: usize =
    memory_range!("DRAM2_UNINIT").end - memory_range!("DRAM2_UNINIT").start;

esp_bootloader_esp_idf::esp_app_desc!();

static SOIL_MOISTURE: SoilMoistureCell = SoilMoistureCell::new();
static BATTERY: BatteryCell = BatteryCell::new();

static LED_CHANNEL: leds::LedChannel = Channel::new();
static EDGE_CHANNEL: button::EdgeChannel = Channel::new();

/// Endpoint 0 (the root endpoint) always runs the hidden Matter system
/// clusters, so the sensor endpoint gets ID 1.
const SOIL_ENDPOINT_ID: u16 = 1;

/// The advertised `SAI`/`SII` (session active/idle intervals) *and* the polling
/// intervals of the Thread Sleepy End Device: the protocol stack's ICD state
/// machine feeds them to the driver, which maps them onto the OpenThread
/// data-poll period. `SII` is the long-idle interval; the stack caps it to the
/// short-idle poll (and advertises that) while the device operates as one.
///
/// The serial number is the chip's factory-burned base MAC, so it is unique
/// per unit without any provisioning. The config must live in static memory
/// because the Matter stack borrows it for as long as the stack itself lives,
/// which is why [`serial_no_from_efuse`] parks its buffer through `mk_static!`.
fn basic_info(serial_no: &'static str) -> BasicInfoConfig<'static> {
    BasicInfoConfig {
        sai: Some(tuning::ACTIVE_POLL_PERIOD_MS),
        sii: Some(tuning::LIT_POLL_PERIOD_MS),
        vendor_name: "Jan XYZ",
        product_name: "Soil Moisture Sensor",
        serial_no,
        ..TEST_DEV_DET
    }
}

/// The eFuse base MAC as 12 uppercase hex digits - within the 32-character
/// limit the Basic Information cluster puts on the serial number attribute.
fn serial_no_from_efuse() -> &'static str {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mac = esp_hal::efuse::base_mac_address();
    let mac = mac.as_bytes();
    let buf: &'static mut [u8; 12] = mk_static!([u8; 12]).init_with([0u8; 12]);
    for (i, byte) in mac.iter().enumerate() {
        buf[i * 2] = HEX[(byte >> 4) as usize];
        buf[i * 2 + 1] = HEX[(byte & 0x0f) as usize];
    }
    core::str::from_utf8(buf).unwrap()
}

/// The Matter answer to "why did this boot happen", from the chip's reset
/// reason. The C6's ROM reports a brown-out with the same code as a power-on,
/// so a sagging cell looks like a power cycle here; the distinction the spec
/// draws between hardware and software watchdogs is kept.
fn boot_reason(reset: Option<SocResetReason>) -> BootReasonEnum {
    use SocResetReason::{ChipPowerOn, CoreDeepSleep, CoreRtcWdt, CoreSw, Cpu0Sw, SysRtcWdt};

    match reset {
        Some(CoreDeepSleep) => BootReasonEnum::Unspecified,
        Some(ChipPowerOn) => BootReasonEnum::PowerOnReboot,
        Some(CoreSw | Cpu0Sw) => BootReasonEnum::SoftwareReset,
        Some(CoreRtcWdt | SysRtcWdt) => BootReasonEnum::HardwareWatchdogReset,
        Some(_) => BootReasonEnum::SoftwareWatchdogReset,
        None => BootReasonEnum::Unspecified,
    }
}

/// esp-backtrace's `custom-halt` hook: the panic message and backtrace have
/// been printed by the time this runs. Recording the panic and resetting is
/// what makes a crash observable - a hang here would drain the cell, report
/// nothing to the controller, and leave the next boot unable to tell a crash
/// from a power cycle.
#[no_mangle]
extern "Rust" fn custom_halt() -> ! {
    store::set_panic_flag();
    esp_hal::system::software_reset()
}

/// The planned end of a boot that sleeps. Never returns: the wake-up is a
/// reboot.
///
/// The watchdog is turned off first. It is configured to pause in sleep, but
/// a sleep that outlasts its timeout must not depend on that: the next boot's
/// `esp_hal::init` turns it off anyway, and this boot arms it again.
fn deep_sleep(lpwr: &mut LowPower<'_>, rtc: &RefCell<Rtc<'_>>, for_ms: u64) -> ! {
    rtc.borrow_mut().rwdt.disable();
    lpwr.set_wakeup_deadline(
        esp_hal::time::Instant::now() + esp_hal::time::Duration::from_millis(for_ms),
    );
    lpwr.sleep_deep(RtcSleepConfig::deep())
}

/// The ICD Management cluster metadata, exactly as served by
/// [`LitIcdMgmtHandler`].
const ICD_MGMT_CLUSTER: Cluster<'static> = LitIcdMgmtHandler::CLUSTER;

/// The ICD Management cluster lives on the root endpoint. `root_endpoint!`
/// cannot forward extra clusters, but the `clusters!(thread ; ...)` form can, so
/// the endpoint is spelled out here. Keeping the ICD state and handler app-local
/// is what lets the decision layer be told about it through a port.
const ROOT_ENDPOINT: Endpoint<'static> = Endpoint {
    id: ROOT_ENDPOINT_ID,
    device_types: devices!(DEV_TYPE_ROOT_NODE),
    clusters: clusters!(thread ; ICD_MGMT_CLUSTER),
    client_clusters: &[],
    unique_id: None,
    semantic_tags: &[],
};

/// The reachability profile the device advertises, from the decision layer's
/// tuning.
const ICD_MODE: IcdModeConfig = IcdModeConfig {
    idle_mode_duration_s: tuning::LIT_IDLE_MODE_DURATION_SECS,
    active_mode_duration_ms: tuning::ACTIVE_HOLD.as_millis() as u32,
    active_mode_threshold_ms: tuning::ACTIVE_THRESHOLD_MS,
    user_active_mode_trigger_hint: 0,
    user_active_mode_trigger_instruction: "",
};

const NODE: Node = Node {
    endpoints: &[
        ROOT_ENDPOINT,
        Endpoint::new(
            SOIL_ENDPOINT_ID,
            devices!(DEV_TYPE_SOIL_SENSOR),
            clusters!(
                desc::DescHandler::CLUSTER,
                SoilMeasurementHandler::CLUSTER,
                soil_sensor_node::power_source::CLUSTER
            ),
        ),
    ],
};

#[esp_rtos::main]
async fn main(s: Spawner) {
    esp_println::logger::init_logger_from_env();
    info!("Starting...");

    heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    heap_allocator!(#[ram(reclaimed)] size: RECLAIMED_RAM);

    let mut peripherals = esp_hal::init(esp_hal::Config::default());

    // `esp_hal::init` turns every watchdog off, and nothing else would ever
    // reset a hung chip: it would keep its radio on until the cell is empty.
    // Armed this early so that the seconds of synchronous initialization below
    // are covered too; the boot loop feeds it through the ports.
    let mut rtc = Rtc::new(peripherals.RTC_TIMER);
    rtc.rwdt.set_timeout(
        RwdtStage::Stage0,
        esp_hal::time::Duration::from_millis(tuning::WATCHDOG_TIMEOUT.as_millis()),
    );
    rtc.rwdt.enable();
    let rtc = RefCell::new(rtc);

    // A long-idle device spends most of its life asleep, and a wake-up is a
    // reboot - so "why did this boot happen" is the first thing worth knowing.
    // The fine reset taxonomy is a reporting concern (BootReason below); the
    // decision layer only needs sleep wake versus reboot.
    //
    // A low-power pad's wake reports as `Ext1` on this chip, not as `Gpio`:
    // the pad's wake path is the one `LP_AON` drives, and the wake-cause bits
    // follow it. Accepting `Gpio` too keeps this true should a future chip
    // report the same path under the other name.
    let reset = esp_hal::system::reset_reason();
    let wake_cause = wakeup_cause();
    let wake = if wake_cause.is_empty() {
        info!("Cold boot: {reset:?}");
        WakeCause::Cold
    } else if wake_cause.contains(WakeupSource::Gpio) || wake_cause.contains(WakeupSource::Ext1) {
        info!("Woken up by the button ({wake_cause:?})");
        WakeCause::Button
    } else {
        info!("Woken up from deep sleep by the timer ({wake_cause:?})");
        WakeCause::Timer
    };

    if store::take_panic_flag() {
        warn!("The previous boot panicked");
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // The button is armed before anything else, and its task runs from this
    // moment rather than with the rest of the boot loop: a press is what may
    // have woken the device, and the Matter stack takes seconds to come up -
    // a release that lands in there would otherwise never be seen, leaving
    // the gesture state machine waiting for it. The edge channel is static,
    // so edges that arrive while the boot is still initializing wait in it.
    let mut button_input = Flex::new(peripherals.GPIO2);
    button_input.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
    button_input.set_output_enable(false);
    button_input.set_input_enable(true);
    if wake == WakeCause::Button && button_input.is_high() {
        // The press that woke the device was over before the pin could be
        // watched: hand the gesture state machine the release it never saw,
        // so the tap still counts as a tap - left waiting, the machine reads
        // the button as held and ends the boot in a factory reset.
        let _ = EDGE_CHANNEL.sender().try_send(ButtonEdge::Rising);
    }
    s.spawn(button::run(button_input, EDGE_CHANNEL.sender()).unwrap());

    // One yield, here on purpose: `main` holds the executor from its first
    // instruction until its first await, and everything before that point is
    // seconds of synchronous initialization. Without this yield the button
    // task would not arm its interrupt until long after a quick tap has
    // ended - the release would be lost, and the gesture state machine, still
    // in its mid-press start, would read the tap as a hold and factory-reset
    // the device ten seconds in.
    embassy_futures::yield_now().await;

    let mut lpwr = LowPower::new(peripherals.LPWR.reborrow());

    // After the yield on purpose: the button task has armed the pin as a wake
    // source by now, so a press still ends a back-off sleep.
    let crashed_boots = store::crashed_boots();
    let host_reset = matches!(
        reset,
        Some(SocResetReason::CoreUsbUart | SocResetReason::CoreUsbJtag)
    );
    let recovery = recover(crashed_boots, wake, host_reset);
    if recovery.after_crash {
        warn!("{crashed_boots} boot(s) in a row ended without a planned end (panic, watchdog or brown-out)");
    }
    store::set_crashed_boots(recovery.crashed_boots);
    if let Some(back_off) = recovery.back_off {
        warn!("Crash loop: sleeping for {back_off} before the next attempt; press the button to try now");
        deep_sleep(&mut lpwr, &rtc, back_off.as_millis());
    }

    // Antenna/RF-switch setup, driven once at boot and held for the firmware's
    // lifetime.
    let _rf_switch_en = Output::new(
        peripherals.GPIO3.reborrow(),
        Level::Low,
        OutputConfig::default(),
    );
    let _antenna_sel = Output::new(
        peripherals.GPIO14.reborrow(),
        Level::High,
        OutputConfig::default(),
    );

    // The protocol stack needs a cryptographically-secure RNG for its whole
    // operational lifetime, and on this chip that requires exclusive access to
    // ADC1 (see README's "Known limitations"). Seed a software CSPRNG from true
    // hardware entropy once, then release ADC1 for the sensors.
    let trng_source =
        esp_hal::rng::TrngSource::new(peripherals.RNG.reborrow(), peripherals.ADC1.reborrow());
    let mut seed = [0u8; 32];
    {
        let trng = esp_hal::rng::Trng::try_new().unwrap();
        trng.read(&mut seed);
    }
    core::mem::drop(trng_source);

    let crypto = default_crypto(ChaCha12Rng::from_seed(seed), DAC_PRIVKEY);

    let mut weak_rand = crypto.weak_rand().unwrap();
    let discriminator = (weak_rand.next_u32() & 0xfff) as u16;

    // Must be stable across reboots: Thread's SRP client registers services
    // keyed by identifiers tied to this EUI64, and a fresh random one each boot
    // makes the same service look like a new host (see AGENTS.md).
    let ieee_eui64 = EspThreadDriver::ieee_eui64();

    // ADC1 is free now: one config hosts both the soil probe and the battery
    // channel, matching the board's shared-ADC-unit constraint.
    let mut adc_config = AdcConfig::new();
    let battery_pin = adc_config
        .enable_pin_with_cal::<_, AdcCalCurve<_>>(peripherals.GPIO0.reborrow(), Attenuation::_11dB);
    let soil_pin = adc_config
        .enable_pin_with_cal::<_, AdcCalCurve<_>>(peripherals.GPIO1.reborrow(), Attenuation::_11dB);
    let adc = Adc::new(peripherals.ADC1.reborrow(), adc_config).into_async();

    // Excitation PWM for the soil probe, idle (0% duty) until a measurement is
    // taken. 6-bit duty rather than the original firmware's 8-bit: the divisor
    // formula undershoots the valid range at 200 kHz / 8-bit unless the APB
    // clock is exactly 80 MHz (see AGENTS.md).
    let mut ledc = Ledc::new(peripherals.LEDC.reborrow());
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let mut excitation_timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    excitation_timer
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty6Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: esp_hal::time::Rate::from_khz(tuning::SOIL_EXCITATION_FREQ_KHZ),
        })
        .unwrap();
    let mut excitation_channel =
        ledc.channel(channel::Number::Channel0, peripherals.GPIO21.reborrow());
    excitation_channel
        .configure(channel::config::Config {
            timer: &excitation_timer,
            duty_pct: 0,
            drive_mode: esp_hal::gpio::DriveMode::PushPull,
        })
        .unwrap();

    let soil_probe = SoilProbe::new(excitation_channel, soil_pin);
    let battery = Battery::new(battery_pin);

    let status_leds = StatusLeds::new(
        Output::new(peripherals.GPIO20, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO18, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO19, Level::Low, OutputConfig::default()),
    );

    // The LED task runs from the very start of the boot, alongside the button
    // task: the wake phase's classification blink has to render before the
    // protocol stack exists, and a blink outlives the moment that asked for
    // it, which is why this is a spawned task and not a future under the boot
    // loop's select.
    s.spawn(leds::run(status_leds, LED_CHANNEL.receiver()).unwrap());

    // Flash: one physical peripheral, shared between the protocol stack's own
    // persistence and the calibration store (see `flash.rs`).
    let mut flash_storage = FlashStorage::new(peripherals.FLASH);
    let mut pt_buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let pt = read_partition_table(&mut flash_storage, &mut pt_buf).unwrap();

    let flash_bus: &'static SharedFlashBus = mk_static!(SharedFlashBus).init_with(
        BlockingMutex::new(RefCell::new(FlashCell::new(flash_storage))),
    );

    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .unwrap()
        .unwrap();
    let nvs_range = nvs.offset()..(nvs.offset() + nvs.len());

    let calib = pt.iter().find(|p| p.label_as_str() == "calib").unwrap();
    let calibration_range = calib.offset()..(calib.offset() + calib.len());

    // The Matter store is shared from here on: the boot loop runs while the
    // stack is still starting up, and both sides persist through it. See
    // `store::SharedKvStore` for the discipline that makes that safe.
    let kv = store::SharedKvStore::share(SeqMapKvBlobStore::new(SharedFlash(flash_bus), nvs_range));

    // Allocate the protocol stack statically - mandatory for the wireless stack
    // variation, and it avoids blowing the program stack.
    let stack = mk_static!(EmbassyThreadMatterStack::<BUMP_SIZE, ()>).init_with(
        EmbassyThreadMatterStack::init(
            mk_static!(BasicInfoConfig).init_with(basic_info(serial_no_from_efuse())),
            BasicCommData {
                password: TEST_DEV_COMM.password,
                discriminator,
            },
            &TEST_DEV_ATT,
        ),
    );

    // Before the stack's event loop runs: the BootReason event is emitted once
    // at its start, and reads what is set here. That happens inside the matter
    // future below, where the stack starts up.

    let soil_handler = SoilMeasurementHandler::new(
        SOIL_ENDPOINT_ID,
        Dataver::new_rand(&mut weak_rand),
        &SOIL_MOISTURE,
    );
    let power_handler = PowerSourceHandler::new(
        SOIL_ENDPOINT_ID,
        Dataver::new_rand(&mut weak_rand),
        &BATTERY,
    );

    // The shared long-idle state: the registrations, the check-in counter and
    // the power mode state machine. Statically allocated, because the handler
    // needs a `'static` reference and the registration array must not transit
    // the stack.
    let lit: &'static LitIcd = mk_static!(LitIcd).init_with(LitIcd::init(
        tuning::CHECK_IN_COUNTER_EPOCH,
        ICD_MODE,
        tuning::SIT_POLL_PERIOD_MS,
    ));
    let icd = lit.icd();

    // NB: `EmptyHandler::chain` wraps LIFO - the *last* chained matcher is
    // evaluated first. The broad root matcher must be chained BEFORE the ICD
    // matcher, otherwise it claims every endpoint-0 cluster and answers ICD
    // Management reads with `EndpointNotFound`.
    let handler = EmptyHandler
        .chain(
            |e, _| e == ROOT_ENDPOINT_ID,
            Async(EmbassyThreadMatterStack::<0, ()>::root_handler(
                &(),
                &mut weak_rand,
            )),
        )
        .chain(
            |e, c| e == ROOT_ENDPOINT_ID && c == ICD_MGMT_CLUSTER.id,
            Async(LitIcdMgmtHandler::new(Dataver::new_rand(&mut weak_rand), lit).adapt()),
        )
        .chain(
            |e, c| e == SOIL_ENDPOINT_ID && c == SoilMeasurementHandler::CLUSTER.id,
            // `.adapt()` takes `self` by value; the boot loop also needs a
            // reference to call `.report(...)`, so adapt a `&_`.
            Async(soil_sensor_node::soil_measurement::HandlerAdaptor(
                &soil_handler,
            )),
        )
        .chain(
            |e, c| e == SOIL_ENDPOINT_ID && c == soil_sensor_node::power_source::CLUSTER.id,
            Async(soil_sensor_node::power_source::HandlerAdaptor(
                &power_handler,
            )),
        )
        .chain(
            |e, c| e == SOIL_ENDPOINT_ID && c == desc::DescHandler::CLUSTER.id,
            Async(desc::DescHandler::new(Dataver::new_rand(&mut weak_rand)).adapt()),
        );

    // The wake phase runs first, and its ports' matter half is `None`: the
    // waking press and the boot's one reading happen before the protocol
    // stack exists, and cannot reach it. The stack's own `startup` wants the
    // stack by `&mut` - its compile-time rule that `run` must not precede or
    // overlap `startup` - and the phase structure is what makes room for
    // that: by the time the stack is asked to start, the wake phase is done
    // and nothing holds the stack.
    let mut esp_ports = EspPorts {
        soil: soil_probe,
        battery,
        adc,
        leds: LED_CHANNEL.sender(),
        edges: EDGE_CHANNEL.receiver(),
        rtc: &rtc,
        icd,
        flash: SharedFlash(flash_bus),
        calibration_range,
        soil_handler: &soil_handler,
        power_handler: &power_handler,
        matter: None,
        boot_at: embassy_time::Instant::now(),
    };

    let boot = Boot {
        wake,
        now: esp_ports.now(),
        after_crash: recovery.after_crash,
    };

    let app = match run_wake(&mut esp_ports, boot).await {
        Ok(app) => app,
        // A factory reset asked for in the wake phase: the ports hold no
        // borrow of the stack here, so the reset's `&mut` meets no objection.
        Err(Outcome::FactoryReset) => {
            warn!("Factory reset requested - erasing Matter fabric state and rebooting");
            stack.reset(&crypto, (NODE, &handler), kv).await.unwrap();
            store::set_crashed_boots(0);
            esp_hal::system::software_reset()
        }
        Err(outcome) => {
            unreachable!("the wake phase cannot end a boot any other way: {outcome:?}")
        }
    };

    // The protocol stack starts up here, after the wake phase. Before it
    // runs, the BootReason event it will emit once is told why this boot
    // happened.
    stack.matter().set_boot_reason(boot_reason(reset));
    stack.startup(&crypto, kv).await.unwrap();

    if stack.matter().has_fabrics() {
        info!(
            "To reset, hold the button for {} s",
            tuning::BUTTON_FACTORY_RESET_HOLD.as_secs()
        );
    }

    // The matter half of the ports attaches between the phases: the
    // operational phase is the only one that speaks through the stack.
    esp_ports.attach(stack.matter(), kv);

    let outcome = {
        // The operational phase owns the ports, and drops them: their borrow
        // of the stack must be gone before the factory reset below takes the
        // stack by `&mut`.
        let mut esp_ports = esp_ports;

        let kv = stack.matter().kv(kv);

        let matter = pin!(stack.run_coex(
            EmbassyThread::new(
                EspThreadDriver::new(peripherals.IEEE802154.reborrow(), peripherals.BT.reborrow(),)
                    .with_rx_queue_size(board::THREAD_RX_QUEUE_SIZE),
                crypto.rand().unwrap(),
                ieee_eui64,
                &kv,
                stack,
                true,
            )
            .with_icd(icd),
            &crypto,
            (NODE, &handler),
            &kv,
            (),
        ));

        // A record that never reaches flash costs one full handshake on the
        // next wake-up, so a failed write is logged, not a reason to crash.
        let persist_resumption = pin!(async {
            if let Err(e) = stack
                .matter()
                .run_persist_resumption(
                    &kv,
                    embassy_time::Duration::from_millis(
                        tuning::RESUMPTION_PERSIST_DEBOUNCE.as_millis(),
                    ),
                )
                .await
            {
                warn!("CASE resumption records persist failed: {e:?}");
            }
            core::future::pending::<()>().await
        });

        match select3(matter, persist_resumption, run_boot(&mut esp_ports, app)).await {
            Either3::First(result) => {
                result.unwrap();
                unreachable!("the protocol stack does not end on its own")
            }
            Either3::Second(()) => unreachable!("the resumption writer never ends"),
            Either3::Third(outcome) => outcome,
        }
    };

    match outcome {
        Outcome::Sleep { for_ms } => {
            store::set_crashed_boots(0);
            deep_sleep(&mut lpwr, &rtc, for_ms);
        }
        Outcome::FactoryReset => {}
    }

    warn!("Factory reset requested - erasing Matter fabric state and rebooting");
    stack.reset(&crypto, (NODE, &handler), kv).await.unwrap();
    store::set_crashed_boots(0);
    esp_hal::system::software_reset()
}
