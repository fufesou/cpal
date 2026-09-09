use std::sync::{Arc, Mutex};

use cidre::sc::stream::{Delegate, DelegateImpl};
use cidre::{define_obj_type, ns, objc, sc};

use crate::{BackendSpecificError, StreamError};

// Stream output and stop notifications can arrive on different queues.
pub(super) type ErrorCallback = Arc<Mutex<Box<dyn FnMut(StreamError) + Send + 'static>>>;

define_obj_type!(pub(super) StreamDelegate + DelegateImpl, ErrorCallback, STREAM_DELEGATE);

impl Delegate for StreamDelegate {}

impl StreamDelegate {
    fn report_stop(&self, error: &ns::Error) {
        (self.inner().lock().unwrap())(stream_stop_error(error));
    }
}

#[objc::add_methods]
impl DelegateImpl for StreamDelegate {
    // Stream-stop notifications are separate from audio sample-buffer delivery.
    // https://developer.apple.com/documentation/screencapturekit/scstreamdelegate/stream(_:didstopwitherror:)
    extern "C" fn impl_stream_did_stop_with_err(
        &mut self,
        _cmd: Option<&objc::Sel>,
        _stream: &sc::Stream,
        error: &ns::Error,
    ) {
        self.report_stop(error);
    }
}

fn stream_stop_error(error: &ns::Error) -> StreamError {
    let err = BackendSpecificError {
        description: format!(
            "ScreenCaptureKit stopped capture: domain={}, code={}, {}",
            error.as_cf().domain(),
            error.code(),
            error.localized_description(),
        ),
    };
    // Only the documented system stop requests recreation. Keep intentional user
    // cancellation distinct so callers can respect it instead of restarting capture.
    // https://developer.apple.com/documentation/screencapturekit/scstreamerror/systemstoppedstream
    // https://developer.apple.com/documentation/screencapturekit/scstreamerror/code/userstopped
    if error.as_cf().domain().equal(sc::error::domain())
        && error.code() == sc::error::code::SYSTEM_STOPPED_STREAM
    {
        StreamError::StreamInterrupted { err }
    } else {
        StreamError::BackendSpecific { err }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cidre::objc::Obj;

    fn capture_error(code: isize) -> cidre::arc::R<ns::Error> {
        // NSErrorDomain is an NSString backed by the framework's CFString constant.
        let domain = unsafe { std::mem::transmute(sc::error::domain()) };
        ns::Error::with_domain(domain, code, None)
    }

    #[test]
    fn system_stop_reaches_error_callback_as_an_interruption() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let callback: ErrorCallback = Arc::new(Mutex::new(Box::new(move |error| {
            sender.send(error).unwrap();
        })));
        let mut delegate = StreamDelegate::with(callback);
        let selector = unsafe { objc::sel_reg_name(c"stream:didStopWithError:".as_ptr().cast()) };
        assert!(delegate.responds_to_sel(selector));
        // The filter uses NSObject's initializer; no capture session is started.
        let filter = unsafe { sc::ContentFilter::cls().new() };
        let stream =
            sc::Stream::with_delegate::<(), _>(&filter, &sc::StreamCfg::new(), delegate.as_ref());
        let stop_error = capture_error(sc::error::code::SYSTEM_STOPPED_STREAM);
        delegate.stream_did_stop_with_err(&stream, &stop_error);
        let error = receiver.try_recv().unwrap();
        assert!(matches!(error, StreamError::StreamInterrupted { .. }));
        assert!(error.to_string().contains("code=-3821"));
        assert!(error
            .to_string()
            .contains("com.apple.ScreenCaptureKit.SCStreamErrorDomain"));
        assert!(error
            .to_string()
            .contains(&stop_error.localized_description().to_string()));
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn explicit_user_stop_is_not_an_automatic_restart_request() {
        let error = stream_stop_error(&capture_error(sc::error::code::USER_STOPPED));
        assert!(matches!(error, StreamError::BackendSpecific { .. }));
        assert!(error.to_string().contains("code=-3817"));
    }

    #[test]
    fn matching_code_from_another_domain_is_not_an_interruption() {
        let error = ns::Error::with_domain(
            ns::ErrorDomain::cocoa(),
            sc::error::code::SYSTEM_STOPPED_STREAM,
            None,
        );
        assert!(matches!(
            stream_stop_error(&error),
            StreamError::BackendSpecific { .. }
        ));
    }
}
