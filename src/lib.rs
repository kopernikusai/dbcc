#![feature(test)]

extern crate test;

use can_dbc::{
    ByteOrder, Message, MessageId, MultiplexIndicator, Signal, SignalExtendedValueType,
    ValueDescription, ValueType, DBC,
};
use codegen::{Enum, Function, Impl, Scope, Struct};
use heck::{CamelCase, ShoutySnakeCase, SnakeCase};
use log::warn;
use socketcan::{EFF_MASK, SFF_MASK};

use std::fmt::Write;

#[cfg(test)]
mod tests {
    use byteorder;
    use byteorder::{ByteOrder, LE};
    use test::{black_box, Bencher};

    #[bench]
    fn bench_read_signal(b: &mut Bencher) {
        const byte_payload: &[u8] = &[
            0x4, 0x2, 0xA, 0xA, 0xF, 0xF, 0xE, 0xE, 0xD, 0xD, 0xA, 0xA, 0xF, 0xF, 0xD, 0xD,
        ];

        b.iter(|| {
            let frame_payload: u64 = LE::read_u64(byte_payload);
            let bit_msk_const = 2u64.saturating_pow(8 as u32) - 1;
            let factor: f64 = test::black_box(2.0);
            let start_bit: u64 = test::black_box(8);
            let offset: f64 = test::black_box(10.0);

            (((frame_payload >> start_bit) & bit_msk_const) as f64) * factor + offset
        });
    }
}

/// Character that is prefixed before type names that are
/// are not starting with an alphabetic char.
const PREFIX_CHAR: char = 'X';

/// Character that is used to replace invalid characters
/// in type names.
const REPLACEMENT_CHAR: char = 'X';

/// Suffix that is append to the raw signal function
const RAW_FN_SUFFIX: &str = "raw_value";

type Result<T> = std::result::Result<T, std::fmt::Error>;

#[derive(Debug)]
pub struct DbccOpt {
    /// Should tokio SocketCan BCM streams be generated.
    /// This requires the `tokio-socketcan-bcm` crate.
    pub with_tokio: bool,
}

pub trait TypeName: ToOwned {
    fn to_type_name(&self) -> Self::Owned;
}

impl TypeName for str {
    fn to_type_name(&self) -> String {
        let mut out = String::with_capacity(self.len() + 1);
        let mut chars = self.chars();
        if let Some(first) = chars.next() {
            if !first.is_alphabetic() && first != '_' {
                warn!("string: {} is prefixed with `{}`", self, PREFIX_CHAR);
                out.push(PREFIX_CHAR);
            }
            out.push(first);
        }

        while let Some(chr) = chars.next() {
            if chr.is_digit(10) || chr.is_alphabetic() || chr == '_' {
                out.push(chr);
            } else {
                warn!(
                    "`{}` character in string: {} is replaced by `{}`",
                    chr, self, REPLACEMENT_CHAR
                );
                out.push(REPLACEMENT_CHAR);
            }
        }

        out
    }
}

fn to_enum_name(message_id: &MessageId, signal_name: &str) -> String {
    format!("{}{}", &signal_name.to_camel_case(), message_id.0)
}

pub fn signal_enum(dbc: &DBC, val_desc: &ValueDescription) -> Option<Enum> {
    if let ValueDescription::Signal {
        ref message_id,
        ref signal_name,
        ref value_descriptions,
    } = val_desc
    {
        let mut sig_enum = Enum::new(&to_enum_name(message_id, signal_name));
        sig_enum.allow("dead_code");
        sig_enum.vis("pub");
        sig_enum.repr("u64");
        sig_enum.derive("Debug");
        sig_enum.derive("Clone");
        sig_enum.derive("Copy");
        sig_enum.derive("PartialEq");
        for desc in value_descriptions {
            sig_enum.new_variant(&desc.b().to_camel_case().to_type_name());
        }

        if let Some(signal) = dbc.signal_by_name(*message_id, signal_name) {
            let decoded_type = signal_decoded_type(dbc, message_id, signal);
            sig_enum.new_variant(&format!("XValue({})", decoded_type));
        } else {
            sig_enum.new_variant("XValue(u64)");
        }
        return Some(sig_enum);
    }
    None
}

pub fn signal_enum_impl_from(dbc: &DBC, val_desc: &ValueDescription) -> Option<Impl> {
    if let ValueDescription::Signal {
        ref message_id,
        ref signal_name,
        ref value_descriptions,
    } = val_desc
    {
        let signal_type = if let Some(signal) = dbc.signal_by_name(*message_id, signal_name) {
            signal_decoded_type(dbc, message_id, signal)
        } else {
            "u64".to_string()
        };

        let enum_name = to_enum_name(message_id, signal_name);
        let mut enum_impl = Impl::new(codegen::Type::new(&enum_name));
        enum_impl.impl_trait(format!("From<{}>", signal_type));

        let from_fn = enum_impl.new_fn("from");
        from_fn.allow("dead_code");
        from_fn.arg("val", codegen::Type::new(&signal_type));
        from_fn.ret(codegen::Type::new("Self"));

        let mut matching = String::new();
        write!(&mut matching, "match val as u64 {{\n").unwrap();
        for value_description in value_descriptions {
            write!(
                &mut matching,
                "    {} => {}::{},\n",
                value_description.a(),
                enum_name,
                value_description.b().to_camel_case().to_type_name()
            )
            .unwrap();
        }
         write!(
            &mut matching,
            "    _ => {}::XValue(val),\n",
            enum_name
        )
        .unwrap();
        write!(&mut matching, "}}").unwrap();

        from_fn.line(matching);

        return Some(enum_impl);
    }
    None
}

pub fn signal_fn_raw(dbc: &DBC, signal: &Signal, message_id: &MessageId) -> Result<Function> {
    let raw_fn_name = format!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);

    let mut signal_fn = codegen::Function::new(&raw_fn_name);
    signal_fn.allow("dead_code");
    signal_fn.vis("pub");
    signal_fn.arg_ref_self();

    let signal_decoded_type = signal_decoded_type(dbc, message_id, signal);
    signal_fn.ret(codegen::Type::new(&signal_decoded_type));

    let default_signal_comment = format!("Read {} signal from can frame", signal.name());
    let signal_comment = dbc
        .signal_comment(message_id, signal.name())
        .unwrap_or(&default_signal_comment);

    let signal_unit = if !signal.unit().is_empty() {
        format!("\nUnit: {}", signal.unit())
    } else {
        String::default()
    };

    signal_fn.doc(&format!("{}{}", signal_comment, signal_unit));

    let read_byte_order = match signal.byte_order() {
        ByteOrder::LittleEndian => "let frame_payload: u64 = LE::read_u64(&self.frame_payload);",
        ByteOrder::BigEndian => "let  frame_payload: u64 = BE::read_u64(&self.frame_payload);",
    };
    signal_fn.line(read_byte_order);

    let bit_msk_const = 2u64.saturating_pow(*signal.signal_size() as u32) - 1;
    let signal_shift = shift_amount(
        *signal.byte_order(),
        *signal.start_bit(),
        *signal.signal_size(),
    );

    let calc = calc_raw(dbc, message_id, signal, signal_shift, bit_msk_const)?;
    signal_fn.line(calc);

    Ok(signal_fn)
}

pub fn signal_fn_enum(signal: &Signal, enum_type: String) -> Result<Function> {
    let mut signal_fn = codegen::Function::new(&signal.name().to_snake_case());
    signal_fn.allow("dead_code");
    signal_fn.vis("pub");
    signal_fn.arg_ref_self();

    signal_fn.ret(enum_type.clone());

    let raw_fn_name = format!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);

    signal_fn.line(format!(
        "{}::from(self.{}())",
        enum_type, raw_fn_name
    ));

    Ok(signal_fn)
}

fn calc_raw(
    dbc: &DBC,
    message_id: &MessageId,
    signal: &Signal,
    signal_shift: u64,
    bit_msk_const: u64,
) -> Result<String> {

    let signal_decoded_type = signal_decoded_type(dbc, message_id, signal);
    let boolean_signal = *signal.signal_size() == 1 && *signal.factor() == 1.0 && *signal.offset() == 0.0;

    let mut calc = String::new();

    // No shift required if start_bit == 0
    let shift = if signal_shift != 0 {
        format!("(frame_payload >> {})", signal_shift)
    } else {
        format!("frame_payload")
    };

    write!(&mut calc, "({} & {:#X})", shift, bit_msk_const)?;

    if !boolean_signal {
       write!(&mut calc, " as {}", signal_decoded_type)?;
    }

    if *signal.factor() != 1.0 {
        write!(&mut calc, " * {:.6}", signal.factor())?;
    }

    if *signal.offset() != 0.0 {
        write!(&mut calc, " + {}{}", signal.offset(), signal_decoded_type)?;
    }

    if boolean_signal {
         write!(&mut calc, " == 1")?;
    }

    Ok(calc)
}

fn signal_decoded_type(dbc: &DBC, message_id: &MessageId, signal: &Signal) -> String {
    if let Some(extended_value_type) = dbc.extended_value_type_for_signal(message_id, signal.name())
    {
        match extended_value_type {
            SignalExtendedValueType::IEEEfloat32Bit => return "f32".to_string(),
            SignalExtendedValueType::IEEEdouble64bit => return "f64".to_string(),
            SignalExtendedValueType::SignedOrUnsignedInteger => (), // Handled below, also part of the Signal itself
        }
    }

    if !(*signal.offset() == 0.0 && *signal.factor() == 1.0 ) {
        return "f64".to_string();
    }

    let prefix_int_sign = match *signal.value_type() {
        ValueType::Signed => "i",
        ValueType::Unsigned => "u",
    };

    match signal.signal_size() {
        _ if *signal.signal_size() == 1 => "bool".to_string(),
        _ if *signal.signal_size() > 1 && *signal.signal_size() <= 8 => {
            format!("{}8", prefix_int_sign).to_string()
        },
        _ if *signal.signal_size() > 8 && *signal.signal_size() <= 16 => {
            format!("{}16", prefix_int_sign).to_string()
        },
        _ if *signal.signal_size() > 16 && *signal.signal_size() <= 32 => {
            format!("{}32", prefix_int_sign).to_string()
        },
        _ => format!("{}64", prefix_int_sign).to_string(),
    }
}

fn shift_amount(byte_order: ByteOrder, start_bit: u64, signal_size: u64) -> u64 {
    match byte_order {
        ByteOrder::LittleEndian => start_bit,
        ByteOrder::BigEndian => 64 - signal_size - ((start_bit / 8) * 8 + (7 - (start_bit % 8))),
    }
}

fn message_const(message: &Message) -> String {
    format!(
        "#[allow(dead_code)]\npub const MESSAGE_ID_{}: u32 = {};",
        message.message_name().to_shouty_snake_case(),
        message.message_id().0
    )
}

fn message_struct(dbc: &DBC, message: &Message) -> Struct {
    let mut message_struct = Struct::new(&message.message_name().to_camel_case());
    if let Some(message_comment) = dbc.message_comment(message.message_id()) {
        message_struct.doc(message_comment);
    }
    message_struct.allow("dead_code");
    message_struct.derive("Debug");
    message_struct.vis("pub");
    message_struct.field("frame_payload", "Vec<u8>");
    message_struct
}

fn message_impl(opt: &DbccOpt, dbc: &DBC, message: &Message) -> Result<Impl> {
    let mut msg_impl = Impl::new(codegen::Type::new(&message.message_name().to_camel_case()));

    let new_fn = msg_impl.new_fn("new");
    new_fn.allow("dead_code");
    new_fn.vis("pub");
    new_fn.arg("mut frame_payload", codegen::Type::new("Vec<u8>"));
    new_fn.line("frame_payload.resize(8, 0);");
    new_fn.line(format!(
        "{} {{ frame_payload }}",
        message.message_name().to_camel_case()
    ));
    new_fn.ret(codegen::Type::new(&message.message_name().to_camel_case()));

    if opt.with_tokio {
        msg_impl.push_fn(message_stream(message));
    }

    for signal in message.signals() {
        if *signal.multiplexer_indicator() != MultiplexIndicator::Plain {
            warn!("Multiplexed signals are currently not supported, the message `{}` signal `{}` will be skipped", message.message_name(), signal.name());
            continue;
        }

        msg_impl.push_fn(signal_fn_raw(dbc, signal, message.message_id())?);

        // Check if this signal can be turned into an enum
        let enum_type = dbc
            .value_descriptions_for_signal(message.message_id(), signal.name())
            .map(|_| to_enum_name(message.message_id(), signal.name()));
        if let Some(enum_type) = enum_type {
            msg_impl.push_fn(signal_fn_enum(signal, enum_type)?);
        }
    }

    Ok(msg_impl)
}

/// Generate message stream using socketcan's Broadcast Manager filters via socketcan-tokio.
fn message_stream(message: &Message) -> Function {
    let mut stream_fn = codegen::Function::new("stream");
    stream_fn.allow("dead_code");
    stream_fn.vis("pub");

    stream_fn.arg("can_interface", codegen::Type::new("&str"));
    stream_fn.arg("ival1", codegen::Type::new("&std::time::Duration"));
    stream_fn.arg("ival2", codegen::Type::new("&std::time::Duration"));

    let ret = format!(
        "std::io::Result<impl Stream<Item = Result<{}, std::io::Error>>>",
        message.message_name().to_camel_case()
    );
    stream_fn.ret(ret);

    stream_fn.line("let socket = BCMSocket::open_nb(&can_interface)?;");

    let message_id = match message.message_id().0 & EFF_MASK {
        0...SFF_MASK => format!(
            "let message_id = CANMessageId::SFF({} as u16);",
            (message.message_id().0 & SFF_MASK).to_string()
        ),
        SFF_MASK...EFF_MASK => format!(
            "let message_id = CANMessageId::EFF({});",
            (message.message_id().0 & EFF_MASK).to_string()
        ),
        _ => unreachable!(),
    };
    stream_fn.line(message_id);

    stream_fn.line("let frame_stream = socket.filter_id_incoming_frames(message_id, ival1.clone(), ival2.clone())?.compat();");
    stream_fn.line(format!(
        "let f = frame_stream.map(|frame| frame.map(|frame| {}::new(frame.data().to_vec())));",
        message.message_name().to_camel_case()
    ));
    stream_fn.line("Ok(f)");

    stream_fn
}

/// Genérate code for reading CAN signals
///
/// Example:
/// ```
/// use dbcc::{can_code_gen, DbccOpt};
/// use std::fs::File;
/// use std::io::prelude::*;
/// use std::path::PathBuf;
/// let mut f = File::open("./examples/j1939.dbc").expect("Failed to open input file");
/// let mut buffer = Vec::new();
/// f.read_to_end(&mut buffer).expect("Failed to read file");
/// let dbc_content = can_dbc::DBC::from_slice(&buffer).expect("Failed to parse DBC file");
/// let opt = DbccOpt { with_tokio: true };
/// let code = can_code_gen(&opt, &dbc_content).expect("Failed to generate rust code");
/// println!("{}", code.to_string());
///```
pub fn can_code_gen(opt: &DbccOpt, dbc: &DBC) -> Result<Scope> {
    let mut scope = Scope::new();
    scope.import("byteorder", "{ByteOrder, LE, BE}");

    if opt.with_tokio {
        scope.import("tokio_socketcan_bcm", "{CANMessageId, BCMSocket}");
        scope.import("futures::stream", "Stream");
        scope.import("futures_util::compat", "Stream01CompatExt");
        scope.import("futures_util::stream", "StreamExt");
    }

    for message in dbc.messages() {
        scope.raw(&message_const(message));
    }

    for value_description in dbc.value_descriptions() {
        if let Some(signal_enum) = signal_enum(dbc, value_description) {
            scope.push_enum(signal_enum);
        }

        if let Some(enum_impl) = signal_enum_impl_from(dbc, value_description) {
            scope.push_impl(enum_impl);
        }
    }

    for message in dbc.messages() {
        scope.push_struct(message_struct(&dbc, message));
        scope.push_impl(message_impl(opt, &dbc, message)?);
    }

    Ok(scope)
}
