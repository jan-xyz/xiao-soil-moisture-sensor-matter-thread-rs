use rs_matter_embassy::matter::dm::DeviceType;

/// Matter Device Library spec assigns Soil Sensor device type `0x0045`,
/// revision 1 (`data_model/1.5/device_types/SoilSensor.xml`). `rs-matter`
/// does not define this constant itself, so we do.
pub const DEV_TYPE_SOIL_SENSOR: DeviceType = DeviceType {
    dtype: 0x0045,
    drev: 1,
};
