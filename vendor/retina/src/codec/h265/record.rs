// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Creates a `HEVCDecoderConfigurationRecord`.

use std::ops::Range;

use bytes::Bytes;

use super::nal::{Pps, Sps, UnitType};

/// A constructed record and parameter sets, all sharing the same underlying
/// allocation by reference count.
pub struct Out {
    pub record: Bytes,
    pub sps: Bytes,
    pub pps: Bytes,
    pub vps: Bytes,
}

/// Creates a `HEVCDecoderConfigurationRecord` for the active PPS, SPS, and VPS.
///
/// Only a single of each of may be active at a time according to H.265, so this
/// should be sufficient. If the active parameter set changes,
/// `retina::VideoFrame::has_new_parameters` will return true.
///
/// Always declares `lengthSizeMinusOne` of 3, meaning that NAL units are
/// prefixed with a 4-byte length.
pub(crate) fn decoder_configuration_record(
    raw_pps: &[u8],
    pps: &Pps,
    raw_sps: &[u8],
    sps: &Sps,
    raw_vps: &[u8],
) -> Out {
    let mut record = Vec::new();

    // unsigned int(8) configurationVersion = 1;
    record.push(1);

    // All 11 bytes of Profile:
    // unsigned int(2) general_profile_space;
    // unsigned int(1) general_tier_flag;
    // unsigned int(5) general_profile_idc;
    // unsigned int(32) general_profile_compatibility_flags;
    // unsigned int(48) general_constraint_indicator_flags;
    let profile = sps.profile();
    record.extend(&profile.0[..]);

    // unsigned int(8) general_level_idc;
    record.push(sps.general_level_idc());

    // bit(4) reserved = ‘1111’b;
    // unsigned int(12) min_spatial_segmentation_idc;
    let min_spatial_segmentation_idc = sps
        .vui()
        .and_then(|v| v.min_spatial_segmentation_idc())
        .unwrap_or(0);
    record.extend(&(0b1111_0000_0000_0000 | min_spatial_segmentation_idc).to_be_bytes()[..]);
    let parallelism_type: u8 = if min_spatial_segmentation_idc == 0 {
        0
    } else {
        match (
            pps.entropy_coding_sync_enabled_flag(),
            pps.tiles_enabled_flag(),
        ) {
            (true, true) => 0,
            (true, false) => 3,
            (false, true) => 2,
            (false, false) => 1,
        }
    };

    // bit(6) reserved = ‘111111’b;
    // unsigned int(2) parallelismType;
    record.push(0b1111_1100 | parallelism_type);

    // bit(6) reserved = ‘111111’b;
    // unsigned int(2) chromaFormat;
    record.push(0b1111_1100 | sps.chroma_format_idc());

    // bit(5) reserved = ‘11111’b;
    // unsigned int(3) bitDepthLumaMinus8;
    // bit(5) reserved = ‘11111’b;
    // unsigned int(3) bitDepthChromaMinus8;
    record.push(0b1111_1000 | sps.bit_depth_luma_minus8());
    record.push(0b1111_1000 | sps.bit_depth_chroma_minus8());

    // bit(16) avgFrameRate;
    record.extend([0, 0]);

    // bit(2) constantFrameRate;
    // bit(3) numTemporalLayers;
    // bit(1) temporalIdNested;
    // unsigned int(2) lengthSizeMinusOne;

    // Note: H.265 section 7.4.3.2.1 states
    // `sps_max_sub_layers_minus1 <= vps_max_sub_layers_minus1`. Declare
    // the more constrained value.
    record.push(
        (sps.max_sub_layers() << 3) | (u8::from(sps.temporal_id_nesting_flag()) << 2) | 0b0011,
    );

    // unsigned int(8) numOfArrays;
    // for (j=0; j < numOfArrays; j++) {
    //   bit(1) array_completeness;
    //   unsigned int(1) reserved = 0;
    //   unsigned int(6) NAL_unit_type;
    //   unsigned int(16) numNalus;
    //   for (i=0; i< numNalus; i++) {
    //     unsigned int(16) nalUnitLength;
    //     bit(8*nalUnitLength) nalUnit;
    //   }
    // }
    record.push(3); // 3 arrays: VPS, SPS, PPS
    let vps_range = append_array(raw_vps, UnitType::VpsNut, &mut record);
    let sps_range = append_array(raw_sps, UnitType::SpsNut, &mut record);
    let pps_range = append_array(raw_pps, UnitType::PpsNut, &mut record);
    let record = Bytes::from(record);
    let vps = record.slice(vps_range);
    let sps = record.slice(sps_range);
    let pps = record.slice(pps_range);

    Out {
        record,
        vps,
        sps,
        pps,
    }
}

fn append_array(nal: &[u8], unit_type: UnitType, record: &mut Vec<u8>) -> Range<usize> {
    record.extend([0b1000_0000 | u8::from(unit_type), 0, 1]);
    record.extend(
        &u16::try_from(nal.len())
            .expect("nalUnitLength must fit in u16")
            .to_be_bytes()[..],
    );
    let start = record.len();
    record.extend_from_slice(nal);
    start..record.len()
}

