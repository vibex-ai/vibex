//
//  Copy.swift
//  Vibex
//
//  The shell's own words, in the app's three languages.
//
//  The Rust app resolves its copy the same way; this mirrors that rule so a
//  scanner hint and a settings sheet read as one product. The shell has only
//  a handful of strings, so they live here rather than in a strings table.
//

import Foundation

enum Copy {
    /// The string for the device language: English, Simplified or
    /// Traditional Chinese.
    static func text(_ en: String, _ zhHans: String, _ zhHant: String) -> String {
        let locale = Locale.current
        guard locale.language.languageCode?.identifier == "zh" else { return en }
        if locale.language.script?.identifier == "Hant" {
            return zhHant
        }
        switch locale.region?.identifier {
        case "TW", "HK", "MO":
            return zhHant
        default:
            return zhHans
        }
    }
}
