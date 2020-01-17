use std::fmt;
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddrV4};

use super::soap;
use crate::errors::{AddAnyPortError, AddPortError, GetExternalIpError, RemovePortError, RequestError};

use crate::common::{self, parsing::RequestReponse, messages, parsing};
use crate::PortMappingProtocol;

/// This structure represents a gateway found by the search functions.
#[derive(Clone, Debug)]
pub struct Gateway {
    /// Socket address of the gateway
    addr: SocketAddrV4,
    /// Control url of the device
    control_url: String,
}

impl Gateway {
    /// Create a new Gateway
    pub fn new(addr: SocketAddrV4, control_url: String) -> Gateway {
        Gateway {
            addr: addr,
            control_url: control_url,
        }
    }

    async fn perform_request(
        &self,
        header: &str,
        body: &str,
        ok: &str,
    ) -> Result<RequestReponse, RequestError> {
        let url = format!("{}", self);
        let text = soap::send_async(&url, soap::Action::new(header), body).await?;
        parsing::parse_response(text, ok)
    }

    /// Get the external IP address of the gateway in a tokio compatible way
    pub async fn get_external_ip(&self) -> Result<Ipv4Addr, GetExternalIpError> {
        let result = self
            .perform_request(
                messages::GET_EXTERNAL_IP_HEADER,
                &messages::format_get_external_ip_message(),
                "GetExternalIPAddressResponse",
            ).await;
        parsing::parse_get_external_ip_response(result)
    }

    /// Get an external socket address with our external ip and any port. This is a convenience
    /// function that calls `get_external_ip` followed by `add_any_port`
    ///
    /// The local_addr is the address where the traffic is sent to.
    /// The lease_duration parameter is in seconds. A value of 0 is infinite.
    ///
    /// # Returns
    ///
    /// The external address that was mapped on success. Otherwise an error.
    pub async fn get_any_address(
        &self,
        protocol: PortMappingProtocol,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<SocketAddrV4, AddAnyPortError> {
        let description = description.to_owned();
        let ip = self.get_external_ip().await?;
        let port  = self.add_any_port(protocol, local_addr, lease_duration, &description).await?;
        Ok(SocketAddrV4::new(ip, port))
    }

    /// Add a port mapping.with any external port.
    ///
    /// The local_addr is the address where the traffic is sent to.
    /// The lease_duration parameter is in seconds. A value of 0 is infinite.
    ///
    /// # Returns
    ///
    /// The external port that was mapped on success. Otherwise an error.
    pub async fn add_any_port(
        &self,
        protocol: PortMappingProtocol,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<u16, AddAnyPortError> {
        // This function first attempts to call AddAnyPortMapping on the IGD with a random port
        // number. If that fails due to the method being unknown it attempts to call AddPortMapping
        // instead with a random port number. If that fails due to ConflictInMappingEntry it retrys
        // with another port up to a maximum of 20 times. If it fails due to SamePortValuesRequired
        // it retrys once with the same port values.

        if local_addr.port() == 0 {
            return Err(AddAnyPortError::InternalPortZeroInvalid);
        }

        let external_port = common::random_port();

        let gateway = self.clone();
        let description = description.to_owned();

        // First, attempt to call the AddAnyPortMapping method.
        let resp = self
            .perform_request(
                messages::ADD_ANY_PORT_MAPPING_HEADER,
                &messages::format_add_any_port_mapping_message(
                    protocol,
                    external_port,
                    local_addr,
                    lease_duration,
                    &description,
                ),
                "AddAnyPortMappingResponse",
            ).await;
        match parsing::parse_add_any_port_mapping_response(resp) {
            Ok(port) => Ok(port),
            Err(None) => {
                // The router does not have the AddAnyPortMapping method.
                // Fall back to using AddPortMapping with a random port.
                gateway.retry_add_random_port_mapping(protocol, local_addr, lease_duration, &description).await
            }
            Err(Some(err)) => Err(err),
        }
    }

    async fn retry_add_random_port_mapping(
        &self,
        protocol: PortMappingProtocol,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<u16, AddAnyPortError> {
        for _ in 0u8..20u8 {
            match self.add_random_port_mapping(protocol, local_addr, lease_duration, &description).await {
                Ok(port) => return Ok(port),
                Err(AddAnyPortError::NoPortsAvailable) => continue,
                e => return e,
            }
        }
        Err(AddAnyPortError::NoPortsAvailable)
    }

    async fn add_random_port_mapping(
        &self,
        protocol: PortMappingProtocol,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<u16, AddAnyPortError> {
        let description = description.to_owned();
        let gateway = self.clone();

        let external_port = common::random_port();
        let res = self.add_port_mapping(protocol, external_port, local_addr, lease_duration, &description).await;
        
        match res {
            Ok(_) => Ok(external_port),
            Err(err) => match parsing::convert_add_random_port_mapping_error(err) {
                Some(err) => Err(err),
                None => gateway.add_same_port_mapping(protocol, local_addr, lease_duration, &description).await
            }
        }
    }

    async fn add_same_port_mapping(
        &self,
        protocol: PortMappingProtocol,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<u16, AddAnyPortError> {
        let res = self
            .add_port_mapping(protocol, local_addr.port(), local_addr, lease_duration, description).await;
        match res {
            Ok(_) => Ok(local_addr.port()),
            Err(err) => Err(parsing::convert_add_same_port_mapping_error(err))
        }
    }

    async fn add_port_mapping(
        &self,
        protocol: PortMappingProtocol,
        external_port: u16,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<(), RequestError> {
        self
            .perform_request(
                messages::ADD_PORT_MAPPING_HEADER,
                &messages::format_add_port_mapping_message(
                    protocol,
                    external_port,
                    local_addr,
                    lease_duration,
                    description,
                ),
                "AddPortMappingResponse",
            ).await?;
        Ok(())
    }

    /// Add a port mapping.
    ///
    /// The local_addr is the address where the traffic is sent to.
    /// The lease_duration parameter is in seconds. A value of 0 is infinite.
    pub async fn add_port(
        &self,
        protocol: PortMappingProtocol,
        external_port: u16,
        local_addr: SocketAddrV4,
        lease_duration: u32,
        description: &str,
    ) -> Result<(), AddPortError> {
        if external_port == 0 {
            return Err(AddPortError::ExternalPortZeroInvalid);
        }
        if local_addr.port() == 0 {
            return Err(AddPortError::InternalPortZeroInvalid);
        }

        let res = self.add_port_mapping(protocol, external_port, local_addr, lease_duration, description).await;
        if let Err(err) = res {
            return Err(parsing::convert_add_port_error(err));
        };
        Ok(())
    }

    /// Remove a port mapping.
    pub async fn remove_port(
        &self,
        protocol: PortMappingProtocol,
        external_port: u16,
    ) -> Result<(), RemovePortError> {
        let res = self
            .perform_request(
                messages::DELETE_PORT_MAPPING_HEADER,
                &messages::format_delete_port_message(protocol, external_port),
                "DeletePortMappingResponse",
            ).await;
        parsing::parse_delete_port_mapping_response(res)
    }
}

impl fmt::Display for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "http://{}{}", self.addr, self.control_url)
    }
}

impl PartialEq for Gateway {
    fn eq(&self, other: &Gateway) -> bool {
        self.addr == other.addr && self.control_url == other.control_url
    }
}

impl Eq for Gateway {}

impl Hash for Gateway {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
        self.control_url.hash(state);
    }
}
