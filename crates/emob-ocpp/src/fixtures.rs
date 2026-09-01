//! Real artefacts, from somebody else's document.
//!
//! # Why a fixture module rather than a constant beside each test
//!
//! Every test in this crate that signs its own message proves that the code
//! agrees with itself, which is not the question. These three come out of the
//! Open Charge Alliance's own application note — the example message of
//! `[OCA SMV §5.2]`, its decoded record, and the key it is published with — and
//! they are shared because several layers of the seam have to agree about the
//! same bytes: the 1.6 nesting, the base64, the record, and the signature over
//! it.
//!
//! The record is a **DZG GSH01.1K2L**, a fifth vendor for this workspace's
//! corpus, and it exercises four things a self-written fixture would not:
//!
//! - the OCPP 1.6 spelling, where a `SignedMeterValueType` is JSON serialised
//!   into a `SampledValue.value` string;
//! - `RV` written as a **quoted string** — `"0.636"`, not `0.636`;
//! - a manufacturer OBIS register, `01-00:98.08.00.FF`, whose direction this
//!   crate therefore refuses to guess;
//! - a top-level `"U"` extension array carrying the *lifetime* register, the
//!   cable resistance and the duration — the fields `[OCMF §Extension Points]`
//!   reserves for a manufacturer, beside the `RD` that actually bills.
//!
//! And the one that matters most: the OCPP message's own `meterStop` is
//! `108814`, the **lifetime** register in watt-hours. The billable quantity is
//! the signed transaction difference, `0.636 kWh`. A CSMS billing
//! `meterStop − meterStart` from the OCPP fields would bill a number nothing
//! signed, off a register that is not the session's.
//!
//! © Open Charge Alliance, CC BY-ND 4.0. Reproduced as a conformance fixture.

/// The OCMF record inside the example message, decoded.
pub const OCA_OCMF: &str = concat!(
    r#"OCMF|{"FV" : "1.0","GI" : "DZG-GSH01.1K2L","GS" : "1DZG0028225179","GV" : "230","PG" : "T96","MV"#,
    r#"" : "DZG","MM" : "GSH01.1K2L","MS" : "1DZG0028225179","MF" : "230","IS" : true,"IT" : "CENTRAL_1"#,
    r#"","ID" : "HRWWBX8","CT" : "EVSEID","CI" : "22BZ3178A0","RD" : [{"TM" : "2023-05-19T15:52:39,000+"#,
    r#"0200 I","TX" : "B","RV" : "0.000","RI" : "01-00:98.08.00.FF","RU" : "kWh","RT" : "DC","EF" : "","#,
    r#""ST" : "G"},{"TM" : "2023-05-19T15:53:58,000+0200 I","TX" : "E","RV" : "0.636","RI" : "01-00:98."#,
    r#"08.00.FF","RU" : "kWh","RT" : "DC","EF" : "","ST" : "G"}],"U" : [{"TM" : "2023-05-19T15:52:39,00"#,
    r#"0+0200 I","TX" : "B","RV" : "108.178","RI" : "01-00:9C.08.00.FF","RU" : "kWh","RT" : "DC","EF" :"#,
    r#" "","ST" : "G"},{"TM" : "2023-05-19T15:53:58,000+0200 I","TX" : "E","RV" : "108.814","RI" : "01-"#,
    r#"00:9C.08.00.FF","RU" : "kWh","RT" : "DC","EF" : "","ST" : "G"},{"TM" : "2023-05-19T15:52:39,000+"#,
    r#"0200 I","TX" : "B","RV" : "0.0022","RI" : "01-00:8C.07.00.FF","RU" : "Ohm","RT" : "DC","EF" : """#,
    r#","ST" : "G"},{"TM" : "2023-05-19T15:53:58,000+0200 I","TX" : "E","RV" : "79","RI" : "01-00:00.08"#,
    r#".06.FF","RU" : "s","RT" : "DC","EF" : "","ST" : "G"}]}|{"SA" : "ECDSA-secp256k1-SHA256","SD" : ""#,
    r#"3045022100D03F319C7AD08AD4F507CAFEF166FFE5FE55778B8686762641FF6DDC084E32A70220635A8936FE6C61AACE"#,
    r#"CBFADE966362BD15B08AEF1093989640FABADC34142E52"}"#,
);

/// The key it is published with: a DER `SubjectPublicKeyInfo` on secp256k1.
pub const OCA_KEY_HEX: &str = concat!(
    "3056301006072A8648CE3D020106052B8104000A034200040A88527E23ED871117491BD435DA048041AAF9B371F6A5C4",
    "C048DCD599D969C3A0ECBF77370F23208E7CA03BD35307CB42F5904A9C75BB7D81B41C053467F558",
);

/// The `SampledValue.value` string exactly as `[OCA SMV §5.2]` sends it.
pub const OCA_1_6_SAMPLED_VALUE: &str = concat!(
    r#"{"signedMeterData":"T0NNRnx7IkZWIiA6ICIxLjAiLCJHSSIgOiAiRFpHLUdTSDAxLjFLMkwiLCJHUyIgOiAiMURaRzAw"#,
    r#"MjgyMjUxNzkiLCJHViIgOiAiMjMwIiwiUEciIDogIlQ5NiIsIk1WIiA6ICJEWkciLCJNTSIgOiAiR1NIMDEuMUsyTCIsIk1T"#,
    r#"IiA6ICIxRFpHMDAyODIyNTE3OSIsIk1GIiA6ICIyMzAiLCJJUyIgOiB0cnVlLCJJVCIgOiAiQ0VOVFJBTF8xIiwiSUQiIDog"#,
    r#"IkhSV1dCWDgiLCJDVCIgOiAiRVZTRUlEIiwiQ0kiIDogIjIyQlozMTc4QTAiLCJSRCIgOiBbeyJUTSIgOiAiMjAyMy0wNS0x"#,
    r#"OVQxNTo1MjozOSwwMDArMDIwMCBJIiwiVFgiIDogIkIiLCJSViIgOiAiMC4wMDAiLCJSSSIgOiAiMDEtMDA6OTguMDguMDAu"#,
    r#"RkYiLCJSVSIgOiAia1doIiwiUlQiIDogIkRDIiwiRUYiIDogIiIsIlNUIiA6ICJHIn0seyJUTSIgOiAiMjAyMy0wNS0xOVQx"#,
    r#"NTo1Mzo1OCwwMDArMDIwMCBJIiwiVFgiIDogIkUiLCJSViIgOiAiMC42MzYiLCJSSSIgOiAiMDEtMDA6OTguMDguMDAuRkYi"#,
    r#"LCJSVSIgOiAia1doIiwiUlQiIDogIkRDIiwiRUYiIDogIiIsIlNUIiA6ICJHIn1dLCJVIiA6IFt7IlRNIiA6ICIyMDIzLTA1"#,
    r#"LTE5VDE1OjUyOjM5LDAwMCswMjAwIEkiLCJUWCIgOiAiQiIsIlJWIiA6ICIxMDguMTc4IiwiUkkiIDogIjAxLTAwOjlDLjA4"#,
    r#"LjAwLkZGIiwiUlUiIDogImtXaCIsIlJUIiA6ICJEQyIsIkVGIiA6ICIiLCJTVCIgOiAiRyJ9LHsiVE0iIDogIjIwMjMtMDUt"#,
    r#"MTlUMTU6NTM6NTgsMDAwKzAyMDAgSSIsIlRYIiA6ICJFIiwiUlYiIDogIjEwOC44MTQiLCJSSSIgOiAiMDEtMDA6OUMuMDgu"#,
    r#"MDAuRkYiLCJSVSIgOiAia1doIiwiUlQiIDogIkRDIiwiRUYiIDogIiIsIlNUIiA6ICJHIn0seyJUTSIgOiAiMjAyMy0wNS0x"#,
    r#"OVQxNTo1MjozOSwwMDArMDIwMCBJIiwiVFgiIDogIkIiLCJSViIgOiAiMC4wMDIyIiwiUkkiIDogIjAxLTAwOjhDLjA3LjAw"#,
    r#"LkZGIiwiUlUiIDogIk9obSIsIlJUIiA6ICJEQyIsIkVGIiA6ICIiLCJTVCIgOiAiRyJ9LHsiVE0iIDogIjIwMjMtMDUtMTlU"#,
    r#"MTU6NTM6NTgsMDAwKzAyMDAgSSIsIlRYIiA6ICJFIiwiUlYiIDogIjc5IiwiUkkiIDogIjAxLTAwOjAwLjA4LjA2LkZGIiwi"#,
    r#"UlUiIDogInMiLCJSVCIgOiAiREMiLCJFRiIgOiAiIiwiU1QiIDogIkcifV19fHsiU0EiIDogIkVDRFNBLXNlY3AyNTZrMS1T"#,
    r#"SEEyNTYiLCJTRCIgOiAiMzA0NTAyMjEwMEQwM0YzMTlDN0FEMDhBRDRGNTA3Q0FGRUYxNjZGRkU1RkU1NTc3OEI4Njg2NzYy"#,
    r#"NjQxRkY2RERDMDg0RTMyQTcwMjIwNjM1QTg5MzZGRTZDNjFBQUNFQ0JGQURFOTY2MzYyQkQxNUIwOEFFRjEwOTM5ODk2NDBG"#,
    r#"QUJBREMzNDE0MkU1MiJ9","encodingMethod":"OCMF","publicKey":"MzA1NjMwMTAwNjA3MkE4NjQ4Q0UzRDAyMDEwN"#,
    r#"jA1MkI4MTA0MDAwQTAzNDIwMDA0MEE4ODUyN0UyM0VEODcxMTE3NDkxQkQ0MzVEQTA0ODA0MUFBRjlCMzcxRjZBNUM0QzA0O"#,
    r#"ERDRDU5OUQ5NjlDM0EwRUNCRjc3MzcwRjIzMjA4RTdDQTAzQkQzNTMwN0NCNDJGNTkwNEE5Qzc1QkI3RDgxQjQxQzA1MzQ2N"#,
    r#"0Y1NTg="}"#,
);
