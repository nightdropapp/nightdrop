#!/bin/bash
#
# ADB Wireless Connection Helper
# Connects to Android device over WiFi using adb
#
# Usage:
#   ./adb-wireless.sh <IP> <PORT>        # Connect to device
#   ./adb-wireless.sh --list             # List connected devices
#   ./adb-wireless.sh --disconnect <IP>  # Disconnect device
#   ./adb-wireless.sh --help             # Show this help
#
# Examples:
#   ./adb-wireless.sh 192.168.1.100 5555
#   ./adb-wireless.sh 10.0.0.50

set -e

# Configuration
ADB="${ADB:-adb}"
DEFAULT_PORT="5555"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Functions
print_header() {
    echo -e "${BLUE}╔════════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║${NC} $1"
    echo -e "${BLUE}╚════════════════════════════════════════════════════════════════╝${NC}"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

print_info() {
    echo -e "${YELLOW}ℹ${NC} $1"
}

print_command() {
    echo -e "${CYAN}$${NC} $1"
}

show_help() {
    cat << 'EOF'
ADB Wireless Connection Helper

USAGE:
  adb-wireless.sh <IP> [PORT]           Connect to Android device over WiFi
  adb-wireless.sh --list                List all connected devices
  adb-wireless.sh --disconnect <IP>     Disconnect a specific device
  adb-wireless.sh --help                Show this help message

ARGUMENTS:
  IP                  IP address of Android device (required for connect)
  PORT                ADB port (optional, default: 5555)

EXAMPLES:
  # Connect to device at 192.168.1.100 on default port 5555
  adb-wireless.sh 192.168.1.100

  # Connect to device at 10.0.0.50 on port 5037
  adb-wireless.sh 10.0.0.50 5037

  # List all connected devices
  adb-wireless.sh --list

  # Disconnect from device
  adb-wireless.sh --disconnect 192.168.1.100

SETUP (First Time Only):
  1. Connect Android device via USB
  2. Enable "Developer Options" in Android Settings
  3. Enable "USB Debugging" in Developer Options
  4. Run: adb tcpip 5555
  5. Run: adb-wireless.sh <device-ip>
  6. Disconnect USB (device will stay connected via WiFi)

TROUBLESHOOTING:
  • Connection refused?
    - Ensure device is on same WiFi network
    - Device may have dropped connection, reconnect USB and retry steps 4-5

  • adb command not found?
    - Install Android SDK: https://developer.android.com/studio
    - Or set ADB environment variable: export ADB=/path/to/adb

  • Can't find device IP?
    - Check router's DHCP client list
    - Or enable WiFi Direct on device and check settings

EOF
}

check_adb() {
    if ! command -v "$ADB" &> /dev/null; then
        print_error "adb command not found"
        echo ""
        echo "Install Android SDK or set ADB environment variable:"
        echo "  export ADB=/path/to/android-sdk/platform-tools/adb"
        exit 1
    fi
    
    if ! "$ADB" version &>/dev/null; then
        print_error "adb is not working correctly"
        exit 1
    fi
}

connect_device() {
    local ip="$1"
    local port="${2:-$DEFAULT_PORT}"
    
    print_header "ADB Wireless Connection"
    
    check_adb
    
    if [ -z "$ip" ]; then
        print_error "IP address required"
        echo "Usage: $0 <IP> [PORT]"
        exit 1
    fi
    
    print_info "Connecting to $ip:$port"
    echo ""
    
    # Attempt connection
    print_command "$ADB connect $ip:$port"
    
    if "$ADB" connect "$ip:$port"; then
        print_success "Connected to $ip:$port"
        echo ""
        
        # Show connected devices
        print_info "Connected devices:"
        echo ""
        "$ADB" devices
        echo ""
        
        # Verify device is responding
        print_command "$ADB -s $ip:$port shell echo 'Connected!'"
        if "$ADB" -s "$ip:$port" shell echo "Connected!" &>/dev/null; then
            print_success "Device is responsive"
            echo ""
            
            print_info "You can now use adb commands:"
            echo "  adb -s $ip:$port shell      # Open shell"
            echo "  adb -s $ip:$port push       # Transfer files"
            echo "  adb -s $ip:$port install   # Install APK"
            echo "  adb -s $ip:$port logcat    # View logs"
        else
            print_error "Device not responding"
            exit 1
        fi
    else
        print_error "Failed to connect to $ip:$port"
        exit 1
    fi
}

list_devices() {
    print_header "Connected ADB Devices"
    
    check_adb
    
    echo ""
    "$ADB" devices
    echo ""
}

disconnect_device() {
    local ip="$1"
    local port="${2:-$DEFAULT_PORT}"
    
    if [ -z "$ip" ]; then
        print_error "IP address required for disconnect"
        echo "Usage: $0 --disconnect <IP> [PORT]"
        exit 1
    fi
    
    print_header "Disconnecting Device"
    
    check_adb
    
    print_info "Disconnecting from $ip:$port"
    
    if "$ADB" disconnect "$ip:$port"; then
        print_success "Disconnected from $ip:$port"
    else
        print_error "Failed to disconnect"
        exit 1
    fi
}

# Main
case "$1" in
    "")
        show_help
        exit 1
        ;;
    --help|-h)
        show_help
        ;;
    --list)
        list_devices
        ;;
    --disconnect)
        disconnect_device "$2" "$3"
        ;;
    *)
        connect_device "$1" "$2"
        ;;
esac
