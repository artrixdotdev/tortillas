/// Make a request-response pair for an actor when the request doesn't have a
/// payload.
///
///  # Examples
/// ```
/// use libtortillas::util::actor_request_response;
///
/// actor_request_response!(
///     pub Request,
///     pub Response,
///     Ping Pong(u8),
///     Ding(u8) Dong(u8),
/// );
///
/// // The same as
/// pub enum Request {
///     Ping,
///     Ding(u8),
/// }
///
/// pub enum Response {
///     Pong(u8),
///     Dong(u8)
/// }
/// ```
#[macro_export]
macro_rules! actor_request_response {
    (
        $(#[$doc:meta])* $req_vis:vis $req_name:ident,
        $res_vis:vis $res_name:ident $(#[$res_meta:meta])?,
        $(
            $(#[$variant_meta:meta])*
            $request:ident $( ( $( $request_args:ty ),* $(,)? ) )?
            $response:ident $( ( $( $response_args:ty ),* $(,)? ) )?
        ),* $(,)?
    ) => {
        // Request enum
        $(#[$doc])*
        $req_vis enum $req_name {
            $(
                $(#[$variant_meta])*
                $request $( ( $( $request_args ),* ) )?,
            )*
        }

        // Response enum
        $(#[$res_meta])?
        $(#[$doc])*
        $res_vis enum $res_name {
            $(
                $(#[$variant_meta])*
                $response $( ( $( $response_args ),* ) )?,
            )*
        }
    };
}
