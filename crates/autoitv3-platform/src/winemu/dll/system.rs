//! The version/system-information structs `DllCall` fills.

use autoitv3_runtime::value::Value;

use crate::winfmt::DllStruct;

use super::WindowsEmulation;
use super::super::*;

impl WindowsEmulation {
    /// Write `OSVERSIONINFO(W/EX)` fields into the struct argument.
    pub(in crate::winemu) fn fill_version_struct(&mut self, extra: &[Value]) -> Option<Value> {
        let handle = struct_handle_arg(extra)?;
        let version = self.version;
        let s = self.struct_any_mut(handle)?;
        if let Some(i) = s.field_alias(&["osversioninfosize", "dwosversioninfosize"]) {
            let size = s.size() as u64;
            s.set_int(i, size);
        }
        if let Some(i) = s.field_alias(&["majorversion", "dwmajorversion"]) {
            s.set_int(i, version.major() as u64);
        }
        if let Some(i) = s.field_alias(&["minorversion", "dwminorversion"]) {
            s.set_int(i, version.minor() as u64);
        }
        if let Some(i) = s.field_alias(&["buildnumber", "dwbuildnumber"]) {
            s.set_int(i, version.build() as u64);
        }
        if let Some(i) = s.field_alias(&["platformid", "dwplatformid"]) {
            s.set_int(i, version.platform_id() as u64);
        }
        if let Some(i) = s.field_alias(&["csdversion", "szcsdversion"]) {
            s.set_string(i, None, version.service_pack());
        }
        if let Some(i) = s.field_alias(&["servicepackmajor", "wservicepackmajor"]) {
            s.set_int(i, version.service_pack_major() as u64);
        }
        if let Some(i) = s.field_alias(&["servicepackminor", "wservicepackminor"]) {
            s.set_int(i, version.service_pack_minor() as u64);
        }
        if let Some(i) = s.field_alias(&["suitemask", "wsuitemask"]) {
            s.set_int(i, version.suite_mask() as u64);
        }
        if let Some(i) = s.field_alias(&["producttype", "wproducttype"]) {
            s.set_int(i, version.product_type() as u64);
        }
        Some(Value::Int(1))
    }

    /// Write `SYSTEM_INFO` fields into the struct argument.
    pub(in crate::winemu) fn fill_system_info(&mut self, extra: &[Value]) -> Option<Value> {
        let handle = struct_handle_arg(extra)?;
        let arch = self.arch;
        let s = self.struct_any_mut(handle)?;
        let x64 = arch.pointer_size() == 8;
        let set = |s: &mut DllStruct, aliases: &[&str], value: u64| {
            if let Some(i) = s.field_alias(aliases) {
                s.set_int(i, value);
            }
        };
        set(s, &["wprocessorarchitecture", "processorarchitecture"], arch.system_info_id() as u64);
        set(s, &["dwpagesize", "pagesize"], 4096);
        set(
            s,
            &["lpminimumapplicationaddress", "dwminapplicationaddress"],
            0x1_0000,
        );
        set(
            s,
            &["lpmaximumapplicationaddress", "dwmaxapplicationaddress"],
            0x7FFF_FFFE_FFFF,
        );
        set(s, &["dwactiveprocessormask", "activeprocessormask"], 0xF);
        set(s, &["dwnumberofprocessors", "numberofprocessors"], 4);
        set(
            s,
            &["dwprocessortype", "processortype"],
            if x64 { 8664 } else { 586 },
        );
        set(
            s,
            &["dwallocationgranularity", "allocationgranularity"],
            65536,
        );
        set(s, &["wprocessorlevel", "processorlevel"], 6);
        set(s, &["wprocessorrevision", "processorrevision"], 0x3A09);
        Some(Value::Int(1))
    }
}
