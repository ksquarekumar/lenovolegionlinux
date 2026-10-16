/*
 * A stand-in for the Legion WMI of the 82WQ firmware, for the legionctl-wmi test in a VM
 * (crates/legionctl/tests/e2e.rs). It has the GUIDs, object ids and buffer layouts of the
 * real \_SB.GZFD (docs/design/10-gap.md), keeps what the driver writes, and prints each
 * write as a Debug line ("LWMI <method> <bytes>") that the test reads from the kernel log
 * (acpi.aml_debug_output=1). Like the EC, it resets the fan table at each mode change and
 * keeps a fan table only in custom mode (0xFF). QEMU loads it with -acpitable.
 */
DefinitionBlock ("", "SSDT", 2, "LGNCTL", "LWMITEST", 1)
{
    Scope (\_SB)
    {
        Device (GZFD)
        {
            Name (_HID, "PNP0C14")
            Name (_UID, "LWMI")
            Name (_WDG, Buffer ()
            {
                // 887B54E3-DDDC-4B2C-8B88-68A26A8835D0 GameZone: WMAA, 1 instance, method
                0xE3, 0x54, 0x7B, 0x88, 0xDC, 0xDD, 0x2C, 0x4B, 0x8B, 0x88, 0x68, 0xA2, 0x6A, 0x88, 0x35, 0xD0,
                0x41, 0x41, 0x01, 0x02,
                // DC2A8805-3A8C-41BA-A6F7-092E0089CD3B Other Mode: WMAE, 1 instance, method
                0x05, 0x88, 0x2A, 0xDC, 0x8C, 0x3A, 0xBA, 0x41, 0xA6, 0xF7, 0x09, 0x2E, 0x00, 0x89, 0xCD, 0x3B,
                0x41, 0x45, 0x01, 0x02,
                // 7A8F5407-CB67-4D6E-B547-39B3BE018154 Capability Data 01: WQA5, 10 instances, data block
                0x07, 0x54, 0x8F, 0x7A, 0x67, 0xCB, 0x6E, 0x4D, 0xB5, 0x47, 0x39, 0xB3, 0xBE, 0x01, 0x81, 0x54,
                0x41, 0x35, 0x0A, 0x00,
                // 92549549-4BDE-4F06-AC04-CE8BF898DBAA fan method: WMAB, 1 instance, method
                0x49, 0x95, 0x54, 0x92, 0xDE, 0x4B, 0x06, 0x4F, 0xAC, 0x04, 0xCE, 0x8B, 0xF8, 0x98, 0xDB, 0xAA,
                0x41, 0x42, 0x01, 0x02,
                // D320289E-8FEA-41E0-86F9-911D83151B5F thermal mode event: notify 0xE7, event
                0x9E, 0x28, 0x20, 0xD3, 0xEA, 0x8F, 0xE0, 0x41, 0x86, 0xF9, 0x91, 0x1D, 0x83, 0x15, 0x1B, 0x5F,
                0xE7, 0x00, 0x01, 0x08,
                // CE6C0974-0407-4F50-88BA-4FC3B6559AD8 Lenovo Utility: WMSK, 1 instance, method
                0x74, 0x09, 0x6C, 0xCE, 0x07, 0x04, 0x50, 0x4F, 0x88, 0xBA, 0x4F, 0xC3, 0xB6, 0x55, 0x9A, 0xD8,
                0x53, 0x4B, 0x01, 0x02
            })

            // thermal mode: 0x02 balanced at start
            Name (MODE, 0x02)
            // the fan table (FSS0-FSS9) and the table the EC sets at each mode change
            Name (FTBL, Buffer () { 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A })
            Name (DFLT, Buffer () { 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A })

            // Other Mode feature values by attribute id (device, feature, mode, type)
            Name (FVID, Package ()
            {
                0x0102FF00, 0x0101FF00, 0x0107FF00, 0x0106FF00, 0x0104FF00,
                0x0202FF00, 0x0201FF00, 0x0203FF00, 0x01020200,
                0x04030001, 0x04030002, 0x04020000
            })
            Name (FVVL, Package ()
            {
                70, 119, 56, 55, 100,
                80, 15, 87, 90,
                2200, 2300, 0
            })

            // Capability Data 01: id, supported, default, step, min, max (u32 each)
            Name (CD01, Package ()
            {
                // ppt_pl1_spl 0x0102FF00: supported 7, default 70, step 1, min 30, max 140
                Buffer () { 0x00, 0xFF, 0x02, 0x01, 0x07, 0x00, 0x00, 0x00, 0x46, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x1E, 0x00, 0x00, 0x00, 0x8C, 0x00, 0x00, 0x00 },
                // ppt_pl2_sppt 0x0101FF00: supported 7, default 119, step 1, min 55, max 190
                Buffer () { 0x00, 0xFF, 0x01, 0x01, 0x07, 0x00, 0x00, 0x00, 0x77, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x37, 0x00, 0x00, 0x00, 0xBE, 0x00, 0x00, 0x00 },
                // ppt_pl1_tau 0x0107FF00: supported 7, default 56, step 1, min 4, max 128
                Buffer () { 0x00, 0xFF, 0x07, 0x01, 0x07, 0x00, 0x00, 0x00, 0x38, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00 },
                // ppt_cpu_cl 0x0106FF00: supported 7, default 55, step 1, min 15, max 135
                Buffer () { 0x00, 0xFF, 0x06, 0x01, 0x07, 0x00, 0x00, 0x00, 0x37, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00, 0x87, 0x00, 0x00, 0x00 },
                // cpu_temp 0x0104FF00: supported 7, default 100, step 1, min 75, max 105
                Buffer () { 0x00, 0xFF, 0x04, 0x01, 0x07, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x4B, 0x00, 0x00, 0x00, 0x69, 0x00, 0x00, 0x00 },
                // gpu_nv_ctgp 0x0202FF00: supported 7, default 80, step 1, min 0, max 115
                Buffer () { 0x00, 0xFF, 0x02, 0x02, 0x07, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x73, 0x00, 0x00, 0x00 },
                // gpu_nv_ppab 0x0201FF00: supported 7, default 15, step 1, min 0, max 25
                Buffer () { 0x00, 0xFF, 0x01, 0x02, 0x07, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0x00, 0x00, 0x00 },
                // gpu_temp 0x0203FF00: supported 7, default 87, step 1, min 75, max 87
                Buffer () { 0x00, 0xFF, 0x03, 0x02, 0x07, 0x00, 0x00, 0x00, 0x57, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x4B, 0x00, 0x00, 0x00, 0x57, 0x00, 0x00, 0x00 },
                // gpu_nv_ac_offset 0x0204FF00: not supported, so the driver shows no attribute
                Buffer () { 0x00, 0xFF, 0x04, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 },
                // an attribute of another device (PSU 0x03010000) that the driver does not show
                Buffer () { 0x00, 0x00, 0x01, 0x03, 0x07, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
                            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00 }
            })

            Method (WQA5, 1, NotSerialized)
            {
                Return (DerefOf (CD01 [Arg0]))
            }

            // GameZone: 43 thermal mode version, 44 set mode, 45 get mode, 0x0D fan max mode
            Method (WMAA, 3, Serialized)
            {
                If ((Arg1 == 43))
                {
                    Return (6)
                }
                If ((Arg1 == 45))
                {
                    Return (MODE)
                }
                If ((Arg1 == 44))
                {
                    Debug = Concatenate ("LWMI GZ44 ", ToHexString (Arg2))
                    MODE = (ToInteger (Arg2) & 0xFF)
                    FTBL = DFLT
                }
                If ((Arg1 == 0x0D))
                {
                    Debug = Concatenate ("LWMI GZ13 ", ToHexString (Arg2))
                }
                Return (Zero)
            }

            // fan method: 5 Fan_Get_Table (u32 FSFL, then FSS0-FSS9 as u32), 6 Fan_Set_Table
            Method (WMAB, 3, Serialized)
            {
                If ((Arg1 == 5))
                {
                    Local0 = Buffer (0x30) {}
                    Local1 = Zero
                    While ((Local1 < 10))
                    {
                        Local0 [(4 + (Local1 * 4))] = DerefOf (FTBL [Local1])
                        Local1++
                    }
                    Return (Local0)
                }
                If ((Arg1 == 6))
                {
                    Debug = Concatenate ("LWMI FAN6 ", ToHexString (Arg2))
                    If (((MODE == 0xFF) && (SizeOf (Arg2) >= 0x1A)))
                    {
                        Local1 = Zero
                        While ((Local1 < 10))
                        {
                            FTBL [Local1] = DerefOf (Arg2 [(6 + (Local1 * 2))])
                            Local1++
                        }
                    }
                }
                Return (Zero)
            }

            // Other Mode: 17 GetFeatureValue, 18 SetFeatureValue (attribute id, value)
            Method (WMAE, 3, Serialized)
            {
                If ((SizeOf (Arg2) < 8))
                {
                    Return (Zero)
                }
                CreateDWordField (Arg2, Zero, FID)
                CreateDWordField (Arg2, 4, FVL)
                Local0 = Match (FVID, MEQ, FID, MTR, Zero, Zero)
                If ((Arg1 == 17))
                {
                    If ((Local0 == Ones))
                    {
                        Return (Zero)
                    }
                    Return (DerefOf (FVVL [Local0]))
                }
                If ((Arg1 == 18))
                {
                    Debug = Concatenate ("LWMI OM18 ", ToHexString (Arg2))
                    If ((Local0 != Ones))
                    {
                        FVVL [Local0] = FVL
                    }
                }
                Return (Zero)
            }

            // Lenovo Utility: 1 GetSupport (25 and 27: the mute LEDs exist), 2 SetFeature
            Method (WMSK, 3, Serialized)
            {
                If ((Arg1 == One))
                {
                    Local0 = ToInteger (Arg2)
                    If ((Local0 == 20))
                    {
                        Return (25)
                    }
                    If ((Local0 == 26))
                    {
                        Return (27)
                    }
                    Return (Zero)
                }
                If ((Arg1 == 2))
                {
                    Debug = Concatenate ("LWMI SK2 ", ToHexString (Arg2))
                }
                Return (Zero)
            }

            // Fn+Q into custom mode, as the EC does it: set the mode, reset the fan table, send
            // the thermal mode event (the test calls it through aml-call)
            Method (FNQ, 0, Serialized)
            {
                MODE = 0xFF
                FTBL = DFLT
                Notify (GZFD, 0xE7)
            }

            // the data of the thermal mode event: the new mode
            Method (_WED, 1, NotSerialized)
            {
                If ((Arg0 == 0xE7))
                {
                    Return (MODE)
                }
                Return (Zero)
            }
        }
    }
}
