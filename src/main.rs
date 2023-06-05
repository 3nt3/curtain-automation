use std::time::Duration;

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
    wifi::{BlockingWifi, EspWifi},
};
use esp_idf_sys::{self as _, EspError}; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
use log::*;

mod config;
use config::APP_CONFIG;

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
    let _wifi = wifi(
        APP_CONFIG.wifi_ssid,
        APP_CONFIG.wifi_password,
        peripherals.modem,
        sysloop,
    );

    // mqtt configuration
    let broker_url = format!(
        "mqtt://{}:{}@{}",
        APP_CONFIG.mqtt_username, APP_CONFIG.mqtt_password, APP_CONFIG.mqtt_host
    );
    let mqtt_config = MqttClientConfiguration::default();
    let mut mqtt_client = EspMqttClient::new(broker_url, &mqtt_config, move |message| {
        on_message_received(message, &mut step_pin, &mut direction_pin)
    })
    .unwrap();

    mqtt_client
        .subscribe("/curtains/#", embedded_svc::mqtt::client::QoS::AtLeastOnce)
        .unwrap();

    // turn off led when connected to everything successfully
    led_pin.set_low().unwrap();

    loop {
        info!("loop");

        std::thread::sleep(Duration::from_secs(1));
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

            let topic = message.topic().unwrap();

            match topic {
                "/curtains/step" => {
                    let payload = String::from_utf8(message.data().to_vec()).unwrap();
                    let steps: i16 = payload.parse().unwrap();

                    // let steps: i16 = payload.parse().unwrap();
                    step_motor(step_pin, direction_pin, steps);
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
    info!("stepping motor {} steps", steps);

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
