use std::fmt;

use rsdb::Protocol;
use rsdb_core::MODES_SAMPLE_RATE_HZ;

/// ADS-B receiver center frequency in Hz.
pub const ADS_B_CENTER_FREQUENCY_HZ: u32 = Protocol::Adsb1090.default_center_frequency_hz();

/// RTL-SDR sample rate for the first-pass ADS-B demodulator in Hz.
pub const RTL_SDR_SAMPLE_RATE_HZ: u32 = MODES_SAMPLE_RATE_HZ;

/// Default manual tuner gain in tenths of dB.
pub const DEFAULT_GAIN_TENTH_DB: i32 = 496;

/// Gain selection for the RTL-SDR tuner.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GainMode {
    /// Let the tuner choose gain automatically.
    Auto,
    /// Set tuner gain in tenths of dB.
    Manual(i32),
}

/// Runtime configuration for an RTL-SDR USB source.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RtlSdrConfig {
    /// Zero-based RTL-SDR device index.
    pub device_index: usize,
    /// Protocol this radio job is configured to receive.
    pub protocol: Protocol,
    /// RF center frequency in Hz.
    pub center_frequency_hz: u32,
    /// I/Q sample rate in Hz.
    pub sample_rate_hz: u32,
    /// Tuner gain mode.
    pub gain: GainMode,
    /// Whether to enable bias-T antenna power.
    pub bias_t: bool,
}

impl Default for RtlSdrConfig {
    fn default() -> Self {
        Self {
            device_index: 0,
            protocol: Protocol::Adsb1090,
            center_frequency_hz: ADS_B_CENTER_FREQUENCY_HZ,
            sample_rate_hz: RTL_SDR_SAMPLE_RATE_HZ,
            gain: GainMode::Manual(DEFAULT_GAIN_TENTH_DB),
            bias_t: false,
        }
    }
}

/// Summary of a detected RTL-SDR USB device.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RtlSdrDeviceInfo {
    /// Zero-based index accepted by [`RtlSdrSource::open`].
    pub index: usize,
    /// USB bus identifier.
    pub bus: String,
    /// USB device address on the bus.
    pub address: u8,
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Manufacturer string, when available from USB descriptors.
    pub manufacturer: Option<String>,
    /// Product string, when available from USB descriptors.
    pub product: Option<String>,
    /// Serial number, when available from USB descriptors.
    pub serial: Option<String>,
}

/// Lists connected RTL-SDR USB devices.
///
/// # Errors
///
/// Returns [`UsbError::Driver`] if USB enumeration fails.
pub fn list_rtl_sdr_devices() -> Result<Vec<RtlSdrDeviceInfo>, UsbError> {
    let devices = rs_rtl::list_devices()?;

    Ok(devices
        .into_iter()
        .enumerate()
        .map(|(index, device)| RtlSdrDeviceInfo {
            index,
            bus: device.bus,
            address: device.address,
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            manufacturer: device.manufacturer,
            product: device.product,
            serial: device.serial,
        })
        .collect())
}

/// Opened RTL-SDR USB source configured for ADS-B I/Q capture.
pub struct RtlSdrSource {
    sdr: rs_rtl::RtlSdr,
}

impl RtlSdrSource {
    /// Opens and configures an RTL-SDR USB device.
    ///
    /// # Errors
    ///
    /// Returns [`UsbError::Driver`] when the device cannot be opened,
    /// initialized, tuned, or configured.
    pub fn open(config: RtlSdrConfig) -> Result<Self, UsbError> {
        let mut sdr = rs_rtl::RtlSdr::open(config.device_index)?;

        sdr.set_center_freq(config.center_frequency_hz)?;
        sdr.set_sample_rate(config.sample_rate_hz)?;

        match config.gain {
            GainMode::Auto => sdr.set_gain_auto()?,
            GainMode::Manual(gain_tenth_db) => sdr.set_gain_manual(gain_tenth_db)?,
        }

        sdr.set_bias_t(config.bias_t)?;

        Ok(Self { sdr })
    }

    /// Starts USB bulk streaming of unsigned interleaved I/Q samples.
    ///
    /// # Errors
    ///
    /// Returns [`UsbError::Driver`] when the streaming thread cannot be
    /// started or the RTL2832U bulk endpoint cannot be reset.
    pub fn start_streaming(&mut self) -> Result<IqStream, UsbError> {
        Ok(IqStream {
            inner: self.sdr.start_streaming()?,
        })
    }

    /// Detected tuner name.
    #[must_use]
    pub fn tuner_name(&self) -> String {
        format!("{:?}", self.sdr.tuner_type())
    }

    /// Actual configured center frequency in Hz.
    #[must_use]
    pub fn center_frequency_hz(&self) -> u32 {
        self.sdr.center_freq()
    }

    /// Actual configured sample rate in Hz.
    #[must_use]
    pub fn sample_rate_hz(&self) -> u32 {
        self.sdr.sample_rate()
    }

    /// Supported manual gain values in tenths of dB.
    #[must_use]
    pub fn supported_gains_tenth_db(&self) -> &[i32] {
        self.sdr.gains()
    }
}

/// Active stream of unsigned interleaved I/Q bytes from an RTL-SDR.
pub struct IqStream {
    inner: rs_rtl::AsyncReadHandle,
}

impl IqStream {
    /// Receives the next I/Q byte chunk, blocking until data arrives.
    #[must_use]
    pub fn recv(&self) -> Option<Vec<u8>> {
        self.inner.recv()
    }

    /// Requests the USB streaming thread to stop.
    pub fn stop(&self) {
        self.inner.stop();
    }

    /// Number of chunks dropped by the driver due to queue backpressure.
    #[must_use]
    pub fn dropped_chunks(&self) -> u64 {
        self.inner.dropped_chunks()
    }
}

/// USB source errors.
#[derive(Debug)]
pub enum UsbError {
    /// Error returned by the RTL-SDR USB driver.
    Driver(rs_rtl::Error),
}

impl fmt::Display for UsbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Driver(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for UsbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Driver(error) => Some(error),
        }
    }
}

impl From<rs_rtl::Error> for UsbError {
    fn from(error: rs_rtl::Error) -> Self {
        Self::Driver(error)
    }
}
