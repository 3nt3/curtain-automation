#![feature(once_cell)]

use std::{
    sync::{Arc, OnceLock, Mutex},
    time::Duration,
};

use embedded_svc::{
    mqtt::client::Event,
    wifi::{AccessPointConfiguration, ClientConfiguration, Configuration},
};
use esp_idf_hal::{
    gpio::{Output, OutputPin, PinDriver},
    peripheral,
    prelude::*,
};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    mqtt::client::{EspMqttClient, EspMqttMessage, MqttClientConfiguration},
    wifi::{BlockingWifi, EspWifi, WifiDeviceId},
};
use esp_idf_sys::{self as _, EspError}; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
use log::*;

mod config;
use config::APP_CONFIG;

static TOPIC_PREFIX: OnceLock<Option<String>> = OnceLock::new();
const STEPS_TO_FULLY_OPEN: i16 = 4600;
static CURRENT_POSITION: once_cell::sync::Lazy<Arc<Mutex<f32>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(0.0)));

fn main() {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_sys::link_patches();
    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();
    unsafe {
        esp_idf_sys::nvs_flash_init();
    }

    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take().unwrap();

    info!("config: {:?}", &APP_CONFIG);

    // setup pins
    let mut led_pin = PinDriver::output(peripherals.pins.gpio2).unwrap();

    let mut step_pin = PinDriver::output(peripherals.pins.gpio22).unwrap();
    let mut direction_pin = PinDriver::output(peripherals.pins.gpio23).unwrap();

    led_pin.set_high().unwrap();

    // connect to wifi
    let wifi = wifi(
        APP_CONFIG.wifi_ssid,
        APP_CONFIG.wifi_password,
        peripherals.modem,
        sysloop,
    )
    .unwrap();

    // mqtt configuration
    let broker_url = format!(
        "mqtt://{}:{}@{}",
        APP_CONFIG.mqtt_username, APP_CONFIG.mqtt_password, APP_CONFIG.mqtt_host
    );
    let mqtt_config = MqttClientConfiguration::default();
    homing_sequence(&mut step_pin, &mut direction_pin);

    let mut mqtt_client = EspMqttClient::new(broker_url, &mqtt_config, move |message| {
        on_message_received(message, &mut step_pin, &mut direction_pin)
    })
    .unwrap();

    // get mac address
    let mac_address = wifi
        .get_mac(WifiDeviceId::Sta)
        .map_err(|why| {
            error!("Error getting mac address: {:?}", why);
        })
        .unwrap();
    let mac_address_str = mac_address
        .to_vec()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<String>>()
        .join(":");

    let topic_prefix = format!("/curtains/{}", mac_address_str);
    TOPIC_PREFIX.set(Some(topic_prefix.clone())).unwrap();

    let topic = format!("{}/#", &topic_prefix);
    info!("subscribing to topic {}", topic);

    mqtt_client
        .subscribe(&topic, embedded_svc::mqtt::client::QoS::AtLeastOnce)
        .unwrap();

    // turn off led when connected to everything successfully
    led_pin.set_low().unwrap();

    loop {
        std::thread::sleep(Duration::from_secs(10));

        mqtt_client.publish(
            format!("{}/position", &topic_prefix).as_str(),
            embedded_svc::mqtt::client::QoS::AtLeastOnce,
            false,
            format!("{}", CURRENT_POSITION.lock().unwrap()).as_bytes(),
        ).unwrap();
    }
}

fn on_message_received<T: OutputPin, U: OutputPin>(
    message: &std::result::Result<Event<EspMqttMessage>, EspError>,
    step_pin: &mut PinDriver<T, Output>,
    direction_pin: &mut PinDriver<U, Output>,
) {
    match message {
        Ok(Event::Received(message)) => {
            info!("Received message: {:?}", message);

            let topic_prefix = TOPIC_PREFIX.get().unwrap().as_ref().unwrap();

            let topic = message.topic().unwrap();
            let topic = topic.replace(topic_prefix.as_str(), "");

            match topic.as_str() {
                "/step" => {
                    let payload = String::from_utf8(message.data().to_vec()).unwrap();
                    let steps: i16 = payload.parse().unwrap();

                    // let steps: i16 = payload.parse().unwrap();
                    step_motor(step_pin, direction_pin, steps);
                }
                "/set-position" => {
                    let payload = String::from_utf8(message.data().to_vec()).unwrap();
                    let position: f32 = payload.parse().unwrap();

                    set_position(step_pin, direction_pin, position);
                }
                _ => {
                    error!("Unknown topic: {:?}", topic);
                }
            }
        }
        Ok(Event::Connected(is_connected)) => {
            info!("Connected: {:?}", is_connected);
        }
        Err(e) => {
            error!("Error receiving message: {:?}", e);
        }
        _ => {
            error!("Unknown message received: {:?}", message);
        }
    }
}

fn step_motor<T: OutputPin, U: OutputPin>(
    step_pin: &mut PinDriver<T, Output>,
    direction_pin: &mut PinDriver<U, Output>,
    steps: i16,
) {
    let step_delay = Duration::from_micros(700);

    // positive is right, negative is left
    if steps > 0 {
        direction_pin.set_high().unwrap();
    } else {
        direction_pin.set_low().unwrap();
    }

    for _ in 0..steps.abs() {
        step_pin.set_high().unwrap();
        std::thread::sleep(step_delay);
        step_pin.set_low().unwrap();
        std::thread::sleep(step_delay);
    }
    let mut current_position = CURRENT_POSITION.lock().unwrap();

    let new_position =
        (*current_position + steps as f32 / STEPS_TO_FULLY_OPEN as f32).clamp(0.0, 1.0);
    info!("current_position: {}, new position: {}", current_position, new_position);
    *current_position = new_position;
}

/// Set the position of the curtains in terms of 0-1
fn set_position<T: OutputPin, U: OutputPin>(
    step_pin: &mut PinDriver<T, Output>,
    direction_pin: &mut PinDriver<U, Output>,
    position: f32,
) {
    let current_position = CURRENT_POSITION.lock().unwrap();
    let current_position_as_steps = (*current_position * STEPS_TO_FULLY_OPEN as f32) as i16;
    drop(current_position);

    let delta_steps = (position * STEPS_TO_FULLY_OPEN as f32) as i16 - current_position_as_steps;

    info!(
        "setting position to {} using {} steps delta",
        position, delta_steps
    );
    step_motor(step_pin, direction_pin, delta_steps);
}

fn homing_sequence<T: OutputPin, U: OutputPin>(
    step_pin: &mut PinDriver<T, Output>,
    direction_pin: &mut PinDriver<U, Output>,
) {
    info!("running homing sequence");

    // TODO: this should use one/two limit switch(es)

    // move left for STEPS_TO_FULLY_OPEN steps, this should open the curtain completely
    // the stepper driver does current limiting so it should be fine to just run it into the end
    // — still very unelegant and sounds horrible when it hits the too early
    step_motor(step_pin, direction_pin, -STEPS_TO_FULLY_OPEN);

    let mut current_position = CURRENT_POSITION.lock().unwrap();
    *current_position = 0.0;
}

fn wifi(
    ssid: &str,
    password: &str,
    modem: impl peripheral::Peripheral<P = esp_idf_hal::modem::Modem> + 'static,
    sysloop: EspSystemEventLoop,
) -> anyhow::Result<Box<EspWifi<'static>>> {
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;

    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;

    info!("Starting wifi...");

    wifi.start()?;

    info!("Scanning...");

    let ap_infos = wifi.scan()?;

    let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

    let channel = if let Some(ours) = ours {
        info!(
            "Found configured access point {} on channel {}",
            ssid, ours.channel
        );
        Some(ours.channel)
    } else {
        info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
        None
    };

    wifi.set_configuration(&Configuration::Mixed(
        ClientConfiguration {
            ssid: ssid.into(),
            password: password.into(),
            channel,
            ..Default::default()
        },
        AccessPointConfiguration {
            ssid: "aptest".into(),
            channel: channel.unwrap_or(1),
            ..Default::default()
        },
    ))?;

    info!("Connecting wifi...");

    wifi.connect()?;

    info!("Waiting for DHCP lease...");

    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi DHCP info: {:?}", ip_info);

    Ok(Box::new(esp_wifi))
}
