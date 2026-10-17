// Everything Swift sees from C: the virtual display shim and the Rust GameStream library, and
// _NSGetExecutablePath (where this program really is: Provider.swift).
#include <mach-o/dyld.h>
#include "VirtualDisplay.h"
#include "GameStream.h"
