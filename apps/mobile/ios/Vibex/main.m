#import <UIKit/UIKit.h>
#import "vibex_mobile.h"

int main(int argc, char *argv[]) {
    @autoreleasepool {
        vibex_ios_initialize_notifications();
        vibex_mobile_main();
    }
    return 0;
}
