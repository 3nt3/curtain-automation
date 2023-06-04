use std::time::Duration;

use anyhow::bail;
use embedded_svc::{
    mqtt::client::Event,
    wifi::{AccessPointConfiguration, AuthMethod, ClientConfiguration, Configuration},
};
use esp_idf_hal::{peripheral, prelude::*};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    mqtt::client::{EspMqttClient, EspMqttMessage, MqttClientConfiguration},
    wifi::{AsyncWifi, BlockingWifi, EspWifi},
};
use esp_idf_sys::{self as _, EspError}; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
use log::*;

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

    let _wifi = wifi(
        "FRITZ!Box 7530 QQ",
        "41988153788532892373",
        peripherals.modem,
        sysloop,
    );

    // mqtt configuration
    let mqtt_user = "curtains";
    let mqtt_password = "m0YO9sTtYomkWuzj";
    let mqtt_host = "homeassistant";
    let broker_url = format!("mqtt://{}:{}@{}", mqtt_user, mqtt_password, mqtt_host);
    let mqtt_config = MqttClientConfiguration::default();
    let mut mqtt_client =
        EspMqttClient::new(broker_url, &mqtt_config, on_message_received).unwrap();

    mqtt_client
        .subscribe("/curtains/#", embedded_svc::mqtt::client::QoS::AtLeastOnce)
        .unwrap();

    loop {
        info!("loop");
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn on_message_received(message: &std::result::Result<Event<EspMqttMessage>, EspError>) {
    match message {
        Ok(Event::Received(message)) => {
            info!("Received message: {:?}", message);
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
