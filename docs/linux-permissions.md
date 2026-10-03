# Linux Permissions Setup

KeyVibes needs read access to Linux input devices (`/dev/input/event*`) to observe keyboard events. This document explains how to set up permissions correctly **without running as root**.

## Why Permissions Are Needed

Linux input devices are protected by default:

```bash
$ ls -l /dev/input/event*
crw-rw---- 1 root input 13, 64 Oct  3 00:00 /dev/input/event0
crw-rw---- 1 root input 13, 65 Oct  3 00:00 /dev/input/event1
```

Only `root` and members of the `input` group can read these devices.

KeyVibes **never requires root** and **never grabs keyboards**. It only observes events.

## Recommended Setup

### 1. Add Your User to the Input Group

On most distributions, add your user to the `input` group:

```bash
sudo usermod -aG input $USER
```

Then **log out and log back in** for the change to take effect.

Verify:

```bash
groups | grep input
```

### 2. Verify Device Access

Test access to a keyboard device:

```bash
keyvibes --list-keyboards
```

If successful, you should see your keyboards listed.

## Alternative: Custom udev Rule

If your distribution doesn't use the `input` group, or you want a more granular setup, create a custom udev rule.

Create `/etc/udev/rules.d/99-keyvibes.rules`:

```udev
# Allow read access to keyboard devices for keyvibes
KERNEL=="event*", SUBSYSTEM=="input", ATTRS{name}=="*keyboard*", MODE="0640", GROUP="input"
```

Reload udev rules:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## What KeyVibes Does NOT Do

KeyVibes intentionally avoids operations that would require elevated privileges:

- ❌ Does NOT grab keyboards (`EVIOCGRAB`)
- ❌ Does NOT inject input (`/dev/uinput`)
- ❌ Does NOT modify system configuration
- ❌ Does NOT require `sudo`
- ❌ Does NOT require setuid
- ❌ Does NOT require capabilities

## Troubleshooting

### "Permission denied" when accessing /dev/input/eventX

**Cause**: Your user isn't in the `input` group, or you haven't logged out/in.

**Solution**:
```bash
# Add to group
sudo usermod -aG input $USER

# Verify (after logout/login)
groups | grep input
```

### "No keyboards found"

**Cause**: KeyVibes looks for devices that advertise keyboard capabilities.

**Solution**: List all input devices to verify:
```bash
cat /proc/bus/input/devices
```

Look for devices with `KEY_A`, `KEY_ENTER`, etc.

### KeyVibes works for built-in keyboard but not USB keyboard

**Cause**: Hotplug may not be working, or the device hasn't been detected yet.

**Solution**: Restart KeyVibes after plugging in the USB keyboard, or check that the udev rule applies to the new device.

### Works on X11 but not Wayland

KeyVibes works identically on both. If you see differences, it's likely a permissions issue. Wayland compositors don't affect evdev access.

## Security Considerations

### Why is read access to /dev/input a sensitive permission?

Reading from `/dev/input/event*` allows observing all keyboard input, including passwords. This is why it's restricted by default.

### Is KeyVibes safe?

KeyVibes:
- Never logs key presses
- Never transmits data over the network
- Never writes keyboard data to disk
- Only counts events for diagnostics (no key identities)
- Runs entirely locally

The application is open source. You can audit the code to verify these claims.

### Should I trust udev rules from the internet?

Always inspect udev rules before installing them. The rule provided here is minimal and only grants read access to keyboard input devices for members of the `input` group.

## Distribution-Specific Notes

### Arch Linux
The `input` group is standard. Add your user and log out/in.

### Ubuntu / Debian
The `input` group is standard. Add your user and log out/in.

### Fedora
The `input` group is standard. Add your user and log out/in.

### NixOS
Use `users.users.<name>.extraGroups = [ "input" ];` in your configuration.

## References

- [Linux Input Subsystem](https://www.kernel.org/doc/html/latest/input/input.html)
- [evdev documentation](https://www.freedesktop.org/software/libevdev/doc/latest/)
- [udev rules syntax](https://www.freedesktop.org/software/systemd/man/udev.html)
