# Hyprland minimizer

This is a little tool that minimizes the currently focused window in Hyprland to the tray. The minimized application gets shown in the waybar tray. The icon title is the current window class or title, if the class is empty, so you might just have to adjust your waybar config to show the appropriate icon. I use it to keep my messenger and mail apps and corporate things in the background, so I still receive notifications. 

Minimizing is done by moving the respective window to a special workspace "minimized". It uses the DBus to notify waybar of these apps and provide a menu to close or restore the app, but don't ask me, how; it handles waybar restarts.

Do with this thatever you want, I don't care, It's vibe coded. Honestly, I don't even know what the code actually does or how it works, and I don't want to, as long as it does what it does.
