//
//  Vibex-Bridging-Header.h
//  Vibex
//
//  Makes the host's C ABI visible to Swift.
//
//  The declarations live in `Headers/VibexMobile.h` so the Rust crate's
//  tests can check them against the exported symbols without parsing Swift.
//

#ifndef VIBEX_BRIDGING_HEADER_H
#define VIBEX_BRIDGING_HEADER_H

#import "VibexMobile.h"

#endif /* VIBEX_BRIDGING_HEADER_H */
