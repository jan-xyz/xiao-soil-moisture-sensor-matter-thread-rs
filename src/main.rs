//! XIAO Soil Moisture Sensor - Matter over Thread (Rust port).
//!
//! Boot sequence follows `rs-matter-embassy`'s `light_thread_coex.rs`
//! example (Thread + BLE-concurrent-commissioning Matter stack) plus
//! `light_wifi_persistent.rs`'s flash-persistence and factory-reset
//! scaffolding, adapted to this board's sensors, button and LEDs.
#![no_std]
#![no_main]
#![recursion_limit = "256"]

use core::pin::pin;

use embassy_embedded_hal::adapter::{BlockingAsync, YieldingAsync};
use embassy_executor::Spawner;
use embassy_futures::join::{join, join5};
use embassy_futures::select::{select, select3, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};

use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_bootloader_esp_idf::partitions::{
    read_partition_table, DataPartitionSubType, PartitionType, PARTITION_TABLE_MAX_LEN,
};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::ram;
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use esp_storage::FlashStorage;

use log::{info, warn};

use rs_matter_embassy::matter::crypto::{default_crypto, Crypto, Rng};
use rs_matter_embassy::matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter_embassy::matter::dm::clusters::decl::soil_measurement::ClusterHandler as _;
use rs_matter_embassy::matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter_embassy::matter::dm::clusters::icd_mgmt::{
    ClusterHandler as _, Icd, IcdMgmtHandler, IcdModeConfig, OperatingModeEnum,
};
use rs_matter_embassy::matter::dm::devices::test::{
    DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET,
};
use rs_matter_embassy::matter::dm::devices::DEV_TYPE_ROOT_NODE;
use rs_matter_embassy::matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter_embassy::matter::dm::{Async, Dataver, EmptyHandler, Endpoint, Node};
use rs_matter_embassy::matter::error::Error;
use rs_matter_embassy::matter::im::subscriptions::Subscriptions;
use rs_matter_embassy::matter::persist::{KvBlobStore, KvBlobStoreAccess};
use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
use rs_matter_embassy::matter::{clusters, devices, BasicCommData, Matter};
use rs_matter_embassy::persist::SeqMapKvBlobStore;
use rs_matter_embassy::stack::rand::rand_core::SeedableRng;
use rs_matter_embassy::stack::rand::ChaCha12Rng;
use rs_matter_embassy::wireless::esp::EspThreadDriver;
use rs_matter_embassy::wireless::{
    EmbassyThread, EmbassyThreadMatterStack, SedHandle, ThreadSedConfig,
};

use tinyrlibc as _;

extern crate alloc;

mod battery;
mod button;
mod calibration;
mod clusters;
mod devices;
mod flash;
mod pins;
mod soil_probe;
mod status_led;

use battery::Battery;
use button::ButtonEvent;
use calibration::Calibration;
use clusters::power_source::{BatteryCell, PowerSourceHandler};
use clusters::soil_measurement::{SoilMeasurementHandler, SoilMoistureCell};
use devices::DEV_TYPE_SOIL_SENSOR;
use flash::{SharedFlash, SharedFlashBus};
use soil_probe::SoilProbe;
use status_led::{LedCommand, StatusLeds};

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit()
    }};
}

/// See the reference example for the rationale on this sizing; retuned
/// upward from its 20000 default because this device hosts two extra
/// hand-written cluster handlers and several extra embassy tasks.
const BUMP_SIZE: usize = 24000;

/// Heap for Thread+BLE plus this app's own async tasks. Retuned upward from
/// the reference example's 100 KiB for the same reason as `BUMP_SIZE`.
const HEAP_SIZE: usize = 110 * 1024;

const RECLAIMED_RAM: usize =
    memory_range!("DRAM2_UNINIT").end - memory_range!("DRAM2_UNINIT").start;

esp_bootloader_esp_idf::esp_app_desc!();

static SOIL_MOISTURE: SoilMoistureCell = SoilMoistureCell::new();
static BATTERY: BatteryCell = BatteryCell::new();

static BUTTON_CHANNEL: button::ButtonChannel = Channel::new();
static LED_CHANNEL: status_led::LedChannel = Channel::new();
static SAMPLE_CHANNEL: Channel<CriticalSectionRawMutex, SampleRequest, 4> = Channel::new();
static FACTORY_RESET: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Application handle for the SED duty cycle: button presses and ICD
/// stay-active requests reopen the responsive window, and `icd_poll_mode_task`
/// switches the idle poll period between the SIT and LIT profiles.
static SED: SedHandle = SedHandle::new(pins::THREAD_SIT_POLL_PERIOD_MS);

/// Endpoint 0 (the root endpoint) always runs the hidden Matter system
/// clusters, so the sensor endpoint gets ID 1.
const SOIL_ENDPOINT_ID: u16 = 1;

const BASIC_INFO: BasicInfoConfig = BasicInfoConfig {
    sai: Some(pins::THREAD_ACTIVE_POLL_PERIOD_MS),
    sii: Some(pins::THREAD_SIT_POLL_PERIOD_MS),
    ..TEST_DEV_DET
};

/// The ICD Management cluster lives on the root endpoint. `root_endpoint!`
/// cannot forward extra clusters, but the `clusters!(thread ; ...)` form can,
/// so the endpoint is spelled out here (the same expansion as the stack's
/// `root_endpoint()`, plus ICD Management). This keeps the ICD state and
/// handler app-local, exactly like the soil/battery clusters.
const ROOT_ENDPOINT: Endpoint<'static> = Endpoint {
    id: ROOT_ENDPOINT_ID,
    device_types: devices!(DEV_TYPE_ROOT_NODE),
    clusters: clusters!(thread ; IcdMgmtHandler::CLUSTER),
    client_clusters: &[],
    unique_id: None,
    semantic_tags: &[],
};

/// Long-Idle-Time ICD timings. This node is a Thread SED, so it advertises the
/// LITS feature and flips between SIT and LIT as clients register/unregister.
const ICD_MODE: IcdModeConfig = IcdModeConfig {
    idle_mode_duration_s: 3_600,
    active_mode_duration_ms: 1_000,
    active_mode_threshold_ms: 300,
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
                clusters::power_source::CLUSTER
            ),
        ),
    ],
};

/// One periodic sample every [`pins::SAMPLE_PERIOD_SECS`] seconds, matching the
/// original firmware's "silent" background sampling cadence.
const SAMPLE_PERIOD: embassy_time::Duration =
    embassy_time::Duration::from_secs(pins::SAMPLE_PERIOD_SECS as u64);

#[derive(Clone, Copy, Debug)]
enum SampleRequest {
    Silent,
    ShowLed,
    Calibrate,
}

#[esp_rtos::main]
async fn main(_s: Spawner) {
    esp_println::logger::init_logger_from_env();
    info!("Starting...");

    heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    heap_allocator!(#[ram(reclaimed)] size: RECLAIMED_RAM);

    let mut peripherals = esp_hal::init(esp_hal::Config::default());

    let timg0 = TimerGroup::new(peripherals.TIMG0);

    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Antenna/RF-switch setup, driven once at boot and held for the
    // firmware's lifetime - same as the original firmware.
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

    // The Matter stack needs a cryptographically-secure RNG for its whole
    // operational lifetime, and on this chip that requires exclusive access
    // to ADC1 (see `esp-hal`'s own `TrngSource` doc: "entropy is sourced
    // from the ADC ... the ADC peripheral must be occupied" - a known,
    // acknowledged-unstable limitation, not a stable guarantee). This board
    // also has exactly one ADC unit, which the soil probe and battery
    // sensing need. We seed a plain software CSPRNG from true hardware
    // entropy once here, then release ADC1 for sensing; see README's
    // "Known limitations".
    //
    // `reseeding_csprng(trng, ...)` is deliberately NOT used here: it stores
    // the `Trng` value for the process lifetime to reseed periodically, so
    // `TrngSource` (and therefore ADC1) could never be released - confirmed
    // on real hardware: dropping `TrngSource` while a `Trng` is still held
    // anywhere panics ("TRNG cannot be disabled while it's in use"), and
    // `reseeding_csprng`'s `ReseedingRng` holds one for as long as `crypto`
    // exists, i.e. for the rest of the program.
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
    // (e.g. the per-fabric "Commissioned" service) keyed by identifiers tied
    // to this EUI64. A fresh random EUI64 every boot re-registers the same
    // logical service under what the SRP server sees as a different host,
    // which it rejects as a name conflict (`OT_ERROR_DUPLICATED`) - this was
    // the actual cause of the device going "Offline" in Home Assistant after
    // any reboot, confirmed by comparing `Registered SRP host <id>` across
    // boots and finding a different id each time. The factory-programmed MAC
    // (EUI-48) converted to EUI-64 (insert 0xFF, 0xFE per the standard
    // conversion) is stable for the life of the chip.
    let mac = esp_hal::efuse::base_mac_address();
    let mac = mac.as_bytes();
    let ieee_eui64 = [mac[0], mac[1], mac[2], 0xFF, 0xFE, mac[3], mac[4], mac[5]];

    // ADC1 is free now: one config hosts both the soil probe (GPIO1) and
    // battery (GPIO0) channels, matching the original firmware's shared
    // ADC-unit constraint (`battery_init` there requires `soil_probe_init`
    // to run first for the same reason).
    let mut adc_config = AdcConfig::new();
    let battery_pin = adc_config
        .enable_pin_with_cal::<_, AdcCalCurve<_>>(peripherals.GPIO0.reborrow(), Attenuation::_11dB);
    let soil_pin = adc_config
        .enable_pin_with_cal::<_, AdcCalCurve<_>>(peripherals.GPIO1.reborrow(), Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1.reborrow(), adc_config).into_async();

    // 200 kHz / 68% duty excitation PWM for the soil probe, idle (0% duty)
    // until a measurement is taken.
    let mut ledc = Ledc::new(peripherals.LEDC.reborrow());
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let mut excitation_timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    excitation_timer
        .configure(timer::config::Config {
            // 6-bit (64 steps) rather than the original firmware's 8-bit:
            // the LEDC divisor formula (src_freq * 256 / (freq * precision))
            // undershoots the valid range at 200 kHz / 8-bit unless the APB
            // clock is 80 MHz; 6-bit stays valid down to a 40 MHz source and
            // is still far finer than the two duty values this firmware
            // ever asks for (0% and 68%).
            duty: timer::config::Duty::Duty6Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: esp_hal::time::Rate::from_khz(pins::SOIL_EXCITATION_FREQ_KHZ),
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

    let mut soil_probe = SoilProbe::new(excitation_channel, soil_pin);
    let mut battery = Battery::new(battery_pin);

    // Status LEDs.
    let status_leds = StatusLeds::new(
        Output::new(
            peripherals.GPIO20.reborrow(),
            Level::Low,
            OutputConfig::default(),
        ),
        Output::new(
            peripherals.GPIO18.reborrow(),
            Level::Low,
            OutputConfig::default(),
        ),
        Output::new(
            peripherals.GPIO19.reborrow(),
            Level::Low,
            OutputConfig::default(),
        ),
    );

    // User button, active low.
    let button_input = Input::new(
        peripherals.GPIO2.reborrow(),
        InputConfig::default().with_pull(Pull::Up),
    );

    // Flash: one physical peripheral, shared between Matter's own
    // persistence and our calibration storage (see `flash.rs`).
    let mut flash_storage = FlashStorage::new(peripherals.FLASH.reborrow());
    let mut pt_buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let pt = read_partition_table(&mut flash_storage, &mut pt_buf).unwrap();

    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .unwrap()
        .unwrap();
    let nvs_range = nvs.offset()..(nvs.offset() + nvs.len());

    let calib = pt.iter().find(|p| p.label_as_str() == "calib").unwrap();
    let calib_range = calib.offset()..(calib.offset() + calib.len());

    let flash_bus: SharedFlashBus =
        Mutex::new(YieldingAsync::new(BlockingAsync::new(flash_storage)));
    let mut calibration = Calibration::load(SharedFlash(&flash_bus), calib_range).await;

    // Allocate the Matter stack statically - mandatory for the wireless
    // stack variation, and avoids blowing the program stack (~35-50 KiB).
    let stack = mk_static!(EmbassyThreadMatterStack::<BUMP_SIZE, ()>).init_with(
        EmbassyThreadMatterStack::init(
            &BASIC_INFO,
            BasicCommData {
                password: TEST_DEV_COMM.password,
                discriminator,
            },
            &TEST_DEV_ATT,
        ),
    );

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

    // `Icd` is `!Sync` (it embeds a blocking mutex over a no-op raw mutex), so
    // it cannot live in a `static`. Leak one instance into the global allocator
    // instead: the handler needs a `'static` reference and the executor is
    // single-threaded. Persistence and mDNS operating-mode publishing are
    // handled by `IcdMgmtHandler`'s lifecycle hooks.
    let icd: &'static Icd =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(Icd::new(1_000, ICD_MODE)));

    // NB: `EmptyHandler::chain` wraps LIFO - the *last* chained matcher is
    // evaluated first. The broad root matcher (`ROOT_ENDPOINT_ID`, any cluster)
    // must be chained BEFORE the ICD matcher, otherwise it is evaluated first,
    // claims every endpoint-0 cluster, and answers ICD Management reads with
    // `EndpointNotFound` (its own chain has no ICD handler).
    let handler = EmptyHandler
        .chain(
            |e, _| e == ROOT_ENDPOINT_ID,
            Async(EmbassyThreadMatterStack::<0, ()>::root_handler(
                &(),
                &mut weak_rand,
            )),
        )
        .chain(
            |e, c| e == ROOT_ENDPOINT_ID && c == IcdMgmtHandler::CLUSTER.id,
            Async(IcdMgmtHandler::new(Dataver::new_rand(&mut weak_rand), icd).adapt()),
        )
        .chain(
            |e, c| e == SOIL_ENDPOINT_ID && c == SoilMeasurementHandler::CLUSTER.id,
            // `.adapt()` takes `self` by value; `sample_worker` also needs a
            // reference to call `.report(...)`, so adapt a `&_` (also a
            // `ClusterHandler`, via the generated blanket impl) instead of
            // moving the owned handler into the chain.
            Async(clusters::soil_measurement::HandlerAdaptor(&soil_handler)),
        )
        .chain(
            |e, c| e == SOIL_ENDPOINT_ID && c == clusters::power_source::CLUSTER.id,
            Async(clusters::power_source::HandlerAdaptor(&power_handler)),
        )
        .chain(
            |e, c| e == SOIL_ENDPOINT_ID && c == desc::DescHandler::CLUSTER.id,
            Async(desc::DescHandler::new(Dataver::new_rand(&mut weak_rand)).adapt()),
        );

    let mut store = SeqMapKvBlobStore::new(SharedFlash(&flash_bus), nvs_range);
    stack.startup(&crypto, &mut store).await.unwrap();

    if stack.matter().has_fabrics() {
        info!(
            "To reset, hold the button for {} s",
            pins::BUTTON_FACTORY_RESET_HOLD.as_secs()
        );
    }

    {
        let kv = stack.matter().kv(&mut store);

        let matter = pin!(stack.run_coex(
            EmbassyThread::new(
                EspThreadDriver::new(peripherals.IEEE802154.reborrow(), peripherals.BT.reborrow())
                    .with_rx_queue_size(pins::THREAD_RX_QUEUE_SIZE),
                crypto.rand().unwrap(),
                ieee_eui64,
                &kv,
                stack,
                true,
            )
            .with_sed(
                ThreadSedConfig {
                    active_poll_period_ms: pins::THREAD_ACTIVE_POLL_PERIOD_MS,
                    active_hold: pins::THREAD_ACTIVE_HOLD,
                    child_timeout_s: Some(pins::THREAD_CHILD_TIMEOUT_S),
                },
                &SED,
            ),
            &crypto,
            (NODE, &handler),
            &kv,
            (),
        ));

        let app = pin!(join5(
            join5(
                button::run(button_input, BUTTON_CHANNEL.sender()),
                status_leds.run(LED_CHANNEL.receiver()),
                sample_worker(
                    &mut soil_probe,
                    &mut battery,
                    &mut adc,
                    &mut calibration,
                    &soil_handler,
                    &power_handler
                ),
                dispatch_button_events(),
                periodic_ticker(),
            ),
            check_in_task(icd, stack.matter(), &crypto, stack.subscriptions(), &kv),
            icd_stay_active_task(icd),
            commissioning_keepalive_task(stack.matter()),
            join(
                icd_poll_mode_task(icd),
                persist_reboot_count_task(stack.matter(), &kv)
            ),
        ));

        match select3(matter, FACTORY_RESET.wait(), app).await {
            Either3::First(result) => result.unwrap(),
            Either3::Second(()) => {}
            Either3::Third(_) => unreachable!("app tasks never complete"),
        }
    }

    warn!("Factory reset requested - erasing Matter fabric state and rebooting");
    stack.reset(&crypto, (NODE, &handler), store).await.unwrap();
    esp_hal::system::software_reset()
}

/// Reads `BUTTON_CHANNEL` and routes each event: sample/calibrate requests
/// go to the sample worker, a factory-reset hold signals the top-level
/// select in `main` to tear down the Matter stack.
async fn dispatch_button_events() -> ! {
    let events = BUTTON_CHANNEL.receiver();
    loop {
        match events.receive().await {
            ButtonEvent::SampleNow => {
                SED.request_active();
                SAMPLE_CHANNEL.send(SampleRequest::ShowLed).await;
            }
            ButtonEvent::Calibrate => {
                SED.request_active();
                SAMPLE_CHANNEL.send(SampleRequest::Calibrate).await;
            }
            ButtonEvent::FactoryReset => FACTORY_RESET.signal(()),
        }
    }
}

/// Silent background sampling, same cadence as the original firmware.
async fn periodic_ticker() -> ! {
    loop {
        Timer::after(SAMPLE_PERIOD).await;
        SAMPLE_CHANNEL.send(SampleRequest::Silent).await;
    }
}

/// Owns the soil probe, battery sensor and shared ADC; the single point of
/// contact with that hardware, so the periodic ticker, the button and the
/// calibration flow never contend over it directly.
async fn sample_worker<'d, SoilPin, BatPin>(
    soil_probe: &mut SoilProbe<'d, SoilPin>,
    battery: &mut Battery<'d, BatPin>,
    adc: &mut Adc<'d, esp_hal::peripherals::ADC1<'d>, esp_hal::Async>,
    calibration: &mut Calibration<'_, 'd>,
    soil_handler: &SoilMeasurementHandler<'_>,
    power_handler: &PowerSourceHandler<'_>,
) -> !
where
    SoilPin: esp_hal::analog::adc::AdcChannel,
    BatPin: esp_hal::analog::adc::AdcChannel,
{
    let leds = LED_CHANNEL.sender();
    let requests = SAMPLE_CHANNEL.receiver();

    loop {
        let request = requests.receive().await;

        if matches!(request, SampleRequest::Calibrate) {
            match calibration.run_flow(soil_probe, adc, leds).await {
                Ok(()) => info!(
                    "Calibration accepted: dry={} mV wet={} mV",
                    calibration.dry_mv(),
                    calibration.wet_mv()
                ),
                Err(e) => warn!("Calibration failed: {e:?}"),
            }
            continue;
        }

        let mv = soil_probe.sample_mv(adc).await;
        let percent = soil_sensor_core::soil_moisture_percent(
            mv as i32,
            calibration.dry_mv(),
            calibration.wet_mv(),
        );
        soil_handler.report(percent);
        if matches!(request, SampleRequest::ShowLed) {
            leds.send(LedCommand::ClassifyMoisture {
                moisture_percent: percent,
            })
            .await;
        }

        let mv = battery.sample_mv(adc).await;
        let percent = soil_sensor_core::battery_percent(
            mv as i32,
            pins::BATTERY_EMPTY_MV,
            pins::BATTERY_FULL_MV,
        );
        power_handler.report(percent, mv);
    }
}

/// Adapts a shared [`KvBlobStoreAccess`] into a raw [`KvBlobStore`], so the ICD
/// Check-In sender can persist its counter through the same store the rest of
/// the stack uses. `send_check_in` only ever calls `store`.
struct AccessStore<'a, K: KvBlobStoreAccess>(&'a K);

impl<K: KvBlobStoreAccess> KvBlobStore for AccessStore<'_, K> {
    fn load<'a>(&mut self, key: u16, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        let mut len = 0;
        let mut found = false;

        self.0.access(|store, scratch| {
            if let Ok(Some(data)) = store.load(key, scratch) {
                len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                found = true;
            }
        });

        Ok(found.then(|| &buf[..len]))
    }

    fn store(&mut self, key: u16, data: &[u8], _buf: &mut [u8]) -> Result<(), Error> {
        self.0
            .access(|store, scratch| store.store(key, data, scratch))
    }

    fn remove(&mut self, key: u16, _buf: &mut [u8]) -> Result<(), Error> {
        self.0.access(|store, scratch| store.remove(key, scratch))
    }
}

/// Records this boot in the persisted `RebootCount` once the node has run for
/// [`pins::REBOOT_COUNT_HEALTHY_AFTER`]. `Matter::startup` only loads the
/// count, and writing it later keeps a boot loop from spending flash writes.
async fn persist_reboot_count_task<K: KvBlobStoreAccess>(matter: &Matter<'_>, kv: K) {
    Timer::after(pins::REBOOT_COUNT_HEALTHY_AFTER).await;

    match matter.persist_reboot_count(kv) {
        Ok(()) => info!("Reboot count {} persisted", matter.reboot_count()),
        Err(e) => warn!("Reboot count persist failed: {e:?}"),
    }
}

/// Keeps the SED responsive while the node is uncommissioned or a commissioning
/// window is open. Commissioning reads/writes run over the operational network
/// *before* the fabric exists, so letting the poll period decay to idle during
/// setup can stall or time out the commissioning exchanges.
async fn commissioning_keepalive_task(matter: &Matter<'_>) -> ! {
    const NUDGE: Duration = Duration::from_secs(10);

    loop {
        Timer::after(NUDGE).await;

        if !matter.has_fabrics() || matter.comm_window_state().is_open_on_all_transports() {
            SED.request_active();
        }
    }
}

/// Follows the controller's ICD setting: while an ICD client is registered
/// (LIT, "Battery Saver") the idle poll period is long; otherwise (SIT,
/// "Standard") it is short. Re-checks on registration changes and periodically
/// as a fallback - the interaction model loads persisted registrations during
/// startup, which can happen after this task first runs.
async fn icd_poll_mode_task(icd: &Icd) -> ! {
    const RECHECK: Duration = Duration::from_secs(60);

    let mut applied: Option<u32> = None;

    loop {
        let poll_period_ms = match icd.operating_mode() {
            OperatingModeEnum::LIT => pins::THREAD_LIT_POLL_PERIOD_MS,
            OperatingModeEnum::SIT => pins::THREAD_SIT_POLL_PERIOD_MS,
        };

        if applied != Some(poll_period_ms) {
            applied = Some(poll_period_ms);
            info!("ICD idle poll period -> {poll_period_ms} ms");
            SED.set_idle_period(poll_period_ms);
        }

        let _ = select(icd.wait_registrations_changed(), Timer::after(RECHECK)).await;
    }
}

/// Reopens the SED active window whenever the controller asks the device to
/// stay active (an ICD `StayActiveRequest`), keeping it reachable for the
/// requested duration.
async fn icd_stay_active_task(icd: &Icd) -> ! {
    loop {
        icd.wait_active_extended().await;
        info!("ICD StayActiveRequest: reopening the SED active window");
        SED.request_active();
    }
}

/// Runs the ICD Check-In sweep: periodically asks the shared [`Icd`] state to
/// message every registered client whose subscription has lapsed. An idle sweep
/// sends nothing - `send_check_in` filters on subscription liveness internally.
async fn check_in_task<C, K, const NS: usize>(
    icd: &Icd,
    matter: &Matter<'_>,
    crypto: C,
    subscriptions: &Subscriptions<NS>,
    kv: K,
) -> !
where
    C: Crypto + Copy,
    K: KvBlobStoreAccess,
{
    let mut buf = [0u8; pins::CHECK_IN_BUF];

    loop {
        Timer::after(pins::CHECK_IN_PERIOD).await;

        match icd
            .send_check_in(matter, crypto, subscriptions, AccessStore(&kv), &mut buf)
            .await
        {
            Ok(()) => info!("ICD Check-In sweep complete"),
            Err(e) => warn!("ICD Check-In sweep failed: {e:?}"),
        }
    }
}
