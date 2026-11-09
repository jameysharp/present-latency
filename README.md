# Measuring input lag with cheap hardware

When you're playing a video game, there are many factors which can contribute
to the game feeling distractingly "slow". For example, the game might spend
too much CPU/GPU time computing the next image to display, or be delayed by
communicating with other players across the internet. But those kinds of delays
are relatively easy to measure with various kinds of profiling tools.

I'm investigating different kinds of delay which have been more difficult to
measure. Specifically:

1. Once the next frame hits the framebuffer, how long does it take before the
   screen visibly changes?
2. When the player moves the mouse, how long does it take for software to
   notice?

These two sources of delay are tricky to measure because they involve
interactions with the physical world. People usually test these delays with
specialized equipment, not readily available to most people. (One blog post
in this style, with many links to other similar projects, is [Measuring input
latency on Linux][marco-nett].)

[marco-nett]: https://marco-nett.de/blog/measuring-input-latency-on-linux-x11-vs-wayland-vrr-dxvk/

This project also measures these delays, but the only hardware it requires is a
cheap USB optical mouse. The trade-off is that it's not as accurate.

# Usage

- Ensure you're using Wayland. I want to support other desktop environments and
  operating systems too but each one is a lot of work.
- [Install Rust][], as well as `pkg-config`, `libwayland-client.so`, and
  `libvulkan.so`. (If you happen to be using [Nix][], you can run `nix-shell` to
  get all the dependencies you need.)
- Plug in a mouse with an optical sensor to the computer that you want to test.
- Place the mouse's sensor against the screen you want to test; make sure it
  can't move, either by laying the monitor flat so the mouse can rest on it
  without sliding, or by securing the mouse in place somehow.
- Run this program using `cargo run`. Press any key to exit.

[Install Rust]: https://rust-lang.org/tools/install/
[Nix]: https://nixos.org/

If you can't use Wayland or Rust, you can also try the [web-based input-lag
test][in-browser] that I wrote first. That's even less accurate, but easier to
try out quickly, and should give you some kind of numbers on any platform.

[in-browser]: https://jamey.thesharps.us/input-lag/

# Challenges

Some mice work better than others. Some screens probably work better than
others. You may find this doesn't work for you at all. If it does work, expect
a mouse optical sensor to be a less reliable test instrument than specialized
equipment with a photodiode.

On my ThinkPad's HiDPI screen this works pretty reliably with a variety of mice,
including a $30 Anker Wireless Vertical Ergonomic Optical Mouse, a $10 Logitech
wired USB mouse, and a no-frills Dell wired USB mouse that I borrowed. (I don't
know how much the latter sells for but we practically found it under the couch
cushions.) I've validated the results against specialized test hardware, and
while the mouse-based test is noticeably more noisy, it's in the right ballpark.

On a larger screen with a lower DPI, the Logitech mouse didn't work for me,
and while the Anker mouse did work, it gave dramatically noisier and sometimes
nonsensical results.

People I've asked to test the browser-based version on Apple Retina displays and
on Windows were not able to get meaningful results, so frankly I have seen this
work well on too few systems to have much confidence about where it does or does
not work.

If you figure anything out about how to make it work more reliably, please open
an issue here or contact me in the fediverse: [@jamey@toot.cat][toot].

[toot]: https://toot.cat/@jamey
