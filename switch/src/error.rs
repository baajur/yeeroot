//! Initialization errors.

use error_chain::*;

error_chain! {
	foreign_links {
		Io(::std::io::Error) #[doc="IO error"];
	}
	links {
		Mining(yee_mining::error::Error, yee_mining::error::ErrorKind) #[doc="Mining error"];
	}
}
