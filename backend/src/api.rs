mod bluetooth;
mod generic;
mod nintendo;
mod playstation;
mod xbox;
use anyhow::Result;
use hidapi::HidApi;
use log::debug;
use serde::{Deserialize, Serialize};
use udev::Enumerator;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Controller {
    pub name: String,
    pub product_id: u16,
    pub vendor_id: u16,
    pub capacity: u8,
    pub status: String,
    pub bluetooth: bool,
}

pub async fn controllers_async() -> Result<Vec<Controller>> {
    // Spawn a tokio blocking task because `get_controllers()` is a blocking API
    let controllers = tokio::task::spawn_blocking(controllers).await??;
    Ok(controllers)
}

pub fn controllers() -> Result<Vec<Controller>> {
    let hidapi = HidApi::new()?;
    let mut controllers: Vec<Controller> = Vec::new();

    // If in debug mode, check if there is a fake controller in /tmp/fake_controller.json
    if cfg!(debug_assertions) {
        if let Ok(file) = std::fs::File::open("/tmp/fake_controller.json") {
            let controller: Controller = serde_json::from_reader(file)?;
            debug!("Found fake controller: {:?}", controller);
            controllers.push(controller);
        }
    }

    // HidApi will return 2 copies of the device when the Nintendo Pro Controller is connected via USB.
    // It will additionally return a 3rd device when the controller is connected via Bluetooth + USB.
    let nintendo_pro_controllers: Vec<_> = hidapi
        .device_list()
        .filter(|device_info| {
            device_info.vendor_id() == nintendo::VENDOR_ID_NINTENDO
                && device_info.product_id() == nintendo::PRODUCT_ID_NINTENDO_PROCON
        })
        .collect();

    if nintendo_pro_controllers.len() == 1 || nintendo_pro_controllers.len() == 2 {
        // When we only get one device, we know it's connected via Bluetooth.
        // When we get two devices, we know it's connected only via USB. Both will report the same data, so we'll just return the first one.
        let controller = nintendo::parse_pro_controller_data(nintendo_pro_controllers[0], &hidapi)?;
        controllers.push(controller);
    } else if nintendo_pro_controllers.len() == 3 {
        // When we get three devices, we know it's connected via USB + Bluetooth.
        // We'll only return the Bluetooth device because the USB devices will not report any data.
        let bt_controller = nintendo_pro_controllers
            .iter()
            .find(|device_info| device_info.interface_number() == -1);

        if let Some(bt_controller) = bt_controller {
            let controller = nintendo::parse_pro_controller_data(bt_controller, &hidapi)?;
            controllers.push(controller);
        }
    }

    // for some reason HidApi's list_devices() is returning multiple instances of the same controller
    // so dedupe by serial number
    let mut xbox_controllers: Vec<_> = hidapi
        .device_list()
        .filter(|device_info| {
            device_info.vendor_id() == xbox::MS_VENDOR_ID
                && (device_info.product_id() == xbox::XBOX_CONTROLLER_PRODUCT_ID
                    || device_info.product_id() == xbox::XBOX_ONE_S_LATEST_FW_PRODUCT_ID
                    || device_info.product_id() == xbox::XBOX_WIRELESS_CONTROLLER_USB_PRODUCT_ID
                    || device_info.product_id() == xbox::XBOX_WIRELESS_CONTROLLER_BT_PRODUCT_ID)
        })
        .collect();
    xbox_controllers.dedup_by(|a, b| a.serial_number() == b.serial_number());
    for device_info in xbox_controllers {
        match (device_info.vendor_id(), device_info.product_id()) {
            (xbox::MS_VENDOR_ID, xbox::XBOX_CONTROLLER_PRODUCT_ID) => {
                debug!("!Found Xbox One S controller: {:?}", device_info);
                let controller = xbox::parse_xbox_controller_data(device_info, &hidapi)?;
                controllers.push(controller);
            }
            (xbox::MS_VENDOR_ID, xbox::XBOX_ONE_S_LATEST_FW_PRODUCT_ID) => {
                debug!("Found Xbox One S controller: {:?}", device_info);
                let controller = xbox::parse_xbox_controller_data(&device_info, &hidapi)?;

                controllers.push(controller);
            }
            (xbox::MS_VENDOR_ID, xbox::XBOX_WIRELESS_CONTROLLER_BT_PRODUCT_ID) => {
                debug!("Found Xbox Series X/S controller: {:?}", device_info);
                let controller = xbox::parse_xbox_controller_data(device_info, &hidapi)?;
                controllers.push(controller);
            }
            _ => {}
        }
    }

    for device_info in hidapi.device_list() {
        match (device_info.vendor_id(), device_info.product_id()) {
            (playstation::DS_VENDOR_ID, playstation::DS_PRODUCT_ID) => {
                debug!("Found DualSense controller: {:?}", device_info);
                let controller =
                    playstation::parse_dualsense_controller_data(device_info, &hidapi)?;

                controllers.push(controller);
            }
            (playstation::DS_VENDOR_ID, playstation::DS4_NEW_PRODUCT_ID) => {
                debug!("Found new DualShock 4 controller: {:?}", device_info);
                let controller =
                    playstation::parse_dualshock_controller_data(device_info, &hidapi)?;

                controllers.push(controller);
            }
            (playstation::DS_VENDOR_ID, playstation::DS4_OLD_PRODUCT_ID) => {
                debug!("Found old DualShock 4 controller: {:?}", device_info);
                let controller =
                    playstation::parse_dualshock_controller_data(device_info, &hidapi)?;

                controllers.push(controller);
            }
            _ => {}
        }
    }

    let mut unknown_controllers: Vec<_> = hidapi
        .device_list()
        .filter(|device_info| {
            device_info.interface_number() == -1
                && !generic::IGNORED_VENDORS.contains(&device_info.vendor_id())
        })
        .collect();
    unknown_controllers.dedup_by(|a, b| a.path() == b.path());
    for device_info in unknown_controllers {
        let controller = generic::get_controller_data(device_info, &hidapi)?;
        controllers.push(controller);
    }

    // for Xbox over USB, hidapi-rs is not finding controllers so fall back to using udev
    let mut enumerator = Enumerator::new()?;
    enumerator.match_subsystem("usb")?;
    for device in enumerator.scan_devices()? {
        let device_vendor_id = match device.property_value("ID_VENDOR_ID") {
            Some(val) => val.to_str().unwrap(),
            None => "0",
        };
        let device_product_id = match device.property_value("ID_MODEL_ID") {
            Some(val) => val.to_str().unwrap(),
            None => "0",
        };
        if device_vendor_id == xbox::MS_VENDOR_ID_STR {
            if device_product_id == xbox::XBOX_CONTROLLER_USB_PRODUCT_ID_STR {
                debug!("Found Xbox One S controller over USB");
                controllers.push(xbox::get_xbox_controller(
                    xbox::XBOX_CONTROLLER_USB_PRODUCT_ID,
                    false,
                )?)
            } else if device_product_id == xbox::XBOX_WIRELESS_CONTROLLER_USB_PRODUCT_ID_STR {
                debug!("Found Xbox Series X/S controller over USB");
                controllers.push(xbox::get_xbox_controller(
                    xbox::XBOX_WIRELESS_CONTROLLER_USB_PRODUCT_ID,
                    false,
                )?)
            }
        }
    }

    Ok(controllers)
}
