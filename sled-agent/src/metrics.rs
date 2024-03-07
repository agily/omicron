// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Metrics produced by the sled-agent for collection by oximeter.

use oximeter::types::MetricsError;
use oximeter::types::ProducerRegistry;
use sled_hardware::Baseboard;
use slog::Logger;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

cfg_if::cfg_if! {
    if #[cfg(target_os = "illumos")] {
        use oximeter_instruments::kstat::link;
        use oximeter_instruments::kstat::CollectionDetails;
        use oximeter_instruments::kstat::Error as KstatError;
        use oximeter_instruments::kstat::KstatSampler;
        use oximeter_instruments::kstat::TargetId;
        use std::collections::BTreeMap;
        use std::sync::Mutex;
    } else {
        use anyhow::anyhow;
    }
}

/// The interval on which we ask `oximeter` to poll us for metric data.
pub(crate) const METRIC_COLLECTION_INTERVAL: Duration = Duration::from_secs(30);

/// The interval on which we sample link metrics.
pub(crate) const LINK_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// An error during sled-agent metric production.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(target_os = "illumos")]
    #[error("Kstat-based metric failure")]
    Kstat(#[source] KstatError),

    #[cfg(not(target_os = "illumos"))]
    #[error("Kstat-based metric failure")]
    Kstat(#[source] anyhow::Error),

    #[error("Failed to insert metric producer into registry")]
    Registry(#[source] MetricsError),

    #[error("Failed to fetch hostname")]
    Hostname(#[source] std::io::Error),

    #[error("Non-UTF8 hostname")]
    NonUtf8Hostname,

    #[error("Missing NULL byte in hostname")]
    HostnameMissingNull,
}

// Basic metadata about the sled agent used when publishing metrics.
#[derive(Clone, Debug)]
#[cfg_attr(not(target_os = "illumos"), allow(dead_code))]
struct SledIdentifiers {
    sled_id: Uuid,
    rack_id: Uuid,
    baseboard: Baseboard,
}

/// Type managing all oximeter metrics produced by the sled-agent.
//
// TODO-completeness: We probably want to get kstats or other metrics in to this
// type from other parts of the code, possibly before the `SledAgent` itself
// exists. This is similar to the storage resources or other objects, most of
// which are essentially an `Arc<Mutex<Inner>>`. It would be nice to avoid that
// pattern, but until we have more statistics, it's not clear whether that's
// worth it right now.
#[derive(Clone, Debug)]
// NOTE: The ID fields aren't used on non-illumos systems, rather than changing
// the name of fields that are not yet used.
#[cfg_attr(not(target_os = "illumos"), allow(dead_code))]
pub struct MetricsManager {
    metadata: Arc<SledIdentifiers>,
    _log: Logger,
    #[cfg(target_os = "illumos")]
    kstat_sampler: KstatSampler,
    // TODO-scalability: We may want to generalize this to store any kind of
    // tracked target, and use a naming scheme that allows us pick out which
    // target we're interested in from the arguments.
    //
    // For example, we can use the link name to do this, for any physical or
    // virtual link, because they need to be unique. We could also do the same
    // for disks or memory. If we wanted to guarantee uniqueness, we could
    // namespace them internally, e.g., `"datalink:{link_name}"` would be the
    // real key.
    #[cfg(target_os = "illumos")]
    tracked_links: Arc<Mutex<BTreeMap<String, TargetId>>>,
    registry: ProducerRegistry,
}

impl MetricsManager {
    /// Construct a new metrics manager.
    ///
    /// This takes a few key pieces of identifying information that are used
    /// when reporting sled-specific metrics.
    pub fn new(
        sled_id: Uuid,
        rack_id: Uuid,
        baseboard: Baseboard,
        log: Logger,
    ) -> Result<Self, Error> {
        let registry = ProducerRegistry::with_id(sled_id);

        cfg_if::cfg_if! {
            if #[cfg(target_os = "illumos")] {
                let kstat_sampler = KstatSampler::new(&log).map_err(Error::Kstat)?;
                registry
                    .register_producer(kstat_sampler.clone())
                    .map_err(Error::Registry)?;
                let tracked_links = Arc::new(Mutex::new(BTreeMap::new()));
            }
        }
        Ok(Self {
            metadata: Arc::new(SledIdentifiers { sled_id, rack_id, baseboard }),
            _log: log,
            #[cfg(target_os = "illumos")]
            kstat_sampler,
            #[cfg(target_os = "illumos")]
            tracked_links,
            registry,
        })
    }

    /// Return a reference to the contained producer registry.
    pub fn registry(&self) -> &ProducerRegistry {
        &self.registry
    }
}

#[cfg(target_os = "illumos")]
impl MetricsManager {
    /// Track metrics for a physical datalink.
    pub async fn track_physical_link(
        &self,
        link_name: impl AsRef<str>,
        interval: Duration,
    ) -> Result<(), Error> {
        let hostname = hostname()?;
        let link = link::PhysicalDataLink {
            rack_id: self.metadata.rack_id,
            sled_id: self.metadata.sled_id,
            serial: self.serial_number(),
            hostname,
            link_name: link_name.as_ref().to_string(),
        };
        let details = CollectionDetails::never(interval);
        let id = self
            .kstat_sampler
            .add_target(link, details)
            .await
            .map_err(Error::Kstat)?;
        self.tracked_links
            .lock()
            .unwrap()
            .insert(link_name.as_ref().to_string(), id);
        Ok(())
    }

    /// Stop tracking metrics for a datalink.
    ///
    /// This works for both physical and virtual links.
    #[allow(dead_code)]
    pub async fn stop_tracking_link(
        &self,
        link_name: impl AsRef<str>,
    ) -> Result<(), Error> {
        let maybe_id =
            self.tracked_links.lock().unwrap().remove(link_name.as_ref());
        if let Some(id) = maybe_id {
            self.kstat_sampler.remove_target(id).await.map_err(Error::Kstat)
        } else {
            Ok(())
        }
    }

    /// Track metrics for a virtual datalink.
    #[allow(dead_code)]
    pub async fn track_virtual_link(
        &self,
        link_name: impl AsRef<str>,
        hostname: impl AsRef<str>,
        interval: Duration,
    ) -> Result<(), Error> {
        let link = link::VirtualDataLink {
            rack_id: self.metadata.rack_id,
            sled_id: self.metadata.sled_id,
            serial: self.serial_number(),
            hostname: hostname.as_ref().to_string(),
            link_name: link_name.as_ref().to_string(),
        };
        let details = CollectionDetails::never(interval);
        self.kstat_sampler
            .add_target(link, details)
            .await
            .map(|_| ())
            .map_err(Error::Kstat)
    }

    // Return the serial number out of the baseboard, if one exists.
    fn serial_number(&self) -> String {
        match &self.metadata.baseboard {
            Baseboard::Gimlet { identifier, .. } => identifier.clone(),
            Baseboard::Unknown => String::from("unknown"),
            Baseboard::Pc { identifier, .. } => identifier.clone(),
        }
    }
}

#[cfg(not(target_os = "illumos"))]
impl MetricsManager {
    /// Track metrics for a physical datalink.
    pub async fn track_physical_link(
        &self,
        _link_name: impl AsRef<str>,
        _interval: Duration,
    ) -> Result<(), Error> {
        Err(Error::Kstat(anyhow!(
            "kstat metrics are not supported on this platform"
        )))
    }

    /// Stop tracking metrics for a datalink.
    ///
    /// This works for both physical and virtual links.
    #[allow(dead_code)]
    pub async fn stop_tracking_link(
        &self,
        _link_name: impl AsRef<str>,
    ) -> Result<(), Error> {
        Err(Error::Kstat(anyhow!(
            "kstat metrics are not supported on this platform"
        )))
    }

    /// Track metrics for a virtual datalink.
    #[allow(dead_code)]
    pub async fn track_virtual_link(
        &self,
        _link_name: impl AsRef<str>,
        _hostname: impl AsRef<str>,
        _interval: Duration,
    ) -> Result<(), Error> {
        Err(Error::Kstat(anyhow!(
            "kstat metrics are not supported on this platform"
        )))
    }
}

// Return the current hostname if possible.
#[cfg(target_os = "illumos")]
fn hostname() -> Result<String, Error> {
    // See netdb.h
    const MAX_LEN: usize = 256;
    let mut out = vec![0u8; MAX_LEN + 1];
    if unsafe {
        libc::gethostname(out.as_mut_ptr() as *mut libc::c_char, MAX_LEN)
    } == 0
    {
        // Split into subslices by NULL bytes.
        //
        // We should have a NULL byte, since we've asked for no more than 255
        // bytes in a 256 byte buffer, but you never know.
        let Some(chunk) = out.split(|x| *x == 0).next() else {
            return Err(Error::HostnameMissingNull);
        };
        let s = std::ffi::CString::new(chunk)
            .map_err(|_| Error::NonUtf8Hostname)?;
        s.into_string().map_err(|_| Error::NonUtf8Hostname)
    } else {
        Err(std::io::Error::last_os_error()).map_err(|_| Error::NonUtf8Hostname)
    }
}
