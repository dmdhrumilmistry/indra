# Adding machines

indra can run agents on other computers too: a laptop, a build box, a Windows PC, a server. You
add one with a single command. There is nothing to configure: no SSH, no open ports, no firewall
rules, no names or paths to type.

## Add a machine in three steps

1. **On the computer with the console** (the "hub"), start it so other machines can reach it:

   ```bash
   indra serve --bind 0.0.0.0:7878
   ```

   Open the console using this computer's network address, for example
   `http://192.168.1.20:7878`. (If you open it as `localhost`, indra still puts the network
   address into the command for you.)

2. **In the console**, go to **Machines** and click **Add a machine**. You get one command for
   Linux or macOS, one for Windows, and a short one for a machine that already has indra. Click
   **Copy** on the one you need.

3. **On the other machine**, open a terminal (PowerShell on Windows), go to the folder agents
   should work in, and paste the command. For example, on Windows:

   ```powershell
   cd D:\work
   $env:INDRA_JOIN="http://192.168.1.20:7878 ABCDE-FGHJK"; irm https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.ps1 | iex
   ```

   It installs indra if needed, joins the hub, and keeps running in the background. You can
   close the terminal.

The console waits on the same page and shows the machine as soon as it joins: its name, system,
CPUs and the agents installed on it (Claude Code, Codex, Gemini). Click **Run an agent there**.

The machine names itself after its computer name, and agents work in the folder where you ran the
command. That is only a starting point: change it any time from the console (see
[Where agents work](#where-agents-work)). To choose either up front, open **Options: name and
folder** before copying the command, or pass them to `indra join`:

```bash
indra join http://192.168.1.20:7878 ABCDE-FGHJK --name win-box --target D:\work
```

The code works once, for an hour. Typing it in lower case is fine.

## What happens on the machine

`indra join` does everything that used to be manual:

- **Finds out about the machine:** its computer name, operating system, CPUs and which agent
  CLIs are installed. The hub shows these; you type none of them.
- **Joins the hub:** it uses the code once and gets its own secret key, saved in an owner-only
  file (`agent.json` in indra's config folder). The hub keeps the key in its encrypted secret
  store and adds the machine to its `indra.toml` (name only: no command, no SSH).
- **Keeps the link running,** and starts it again after a log-in or reboot:

  | System | How |
  |---|---|
  | Linux | a systemd user service, `indra-node` (it starts at boot when the system allows it); without systemd, a background process until the next reboot |
  | macOS | a launchd agent, `dev.indra.node`, started at log-in |
  | Windows | a background process with no window, started again when you sign in |

  No administrator rights are needed on any of them. Pass `--foreground` to stay in the
  terminal instead.

**If a machine or the hub goes down, they find each other again by themselves.** The link
reconnects as soon as the hub answers (it retries every few seconds, then every minute). If the
link process itself stops, the background service starts it again (systemd and launchd do; on
Windows, and on Linux without systemd, a small supervisor does). If the hub is restarted with a
different `indra.toml` that does not list the machine, the machine keeps trying once a minute
and reconnects when the hub is back on its own settings. It stops only when you remove it in the
console, or run `indra node leave` on it.

The link is outbound only: the machine connects to the hub, so it needs no open port and no SSH
account, and works from behind a home router. Hub and machine then talk directly over an
encrypted WebRTC channel when they can, and through the hub when they cannot. If the network
drops, the machine reconnects by itself and running agents lose no output.

## Where agents work

Each machine has a list of folders agents may work in. Agents start only in those folders, and
they do not have to be where indra is installed or where you ran the join command.

To change them, click **Change** next to **Folders agents work in** on the machine's card:

- **Browse...** walks the machine's folders (it shows folder names only, never files). Open
  the one you want and click **Use this folder**. **Up** goes one level up; **Home** and, on
  Windows, each drive (`C:\`, `D:\`) jump straight there.
- Or type a full path (`D:\code\my-app`, `/home/me/code/my-app`) and click **Add**.
- **Remove** takes a folder off the list. **Save** applies the change.

The machine checks that each folder exists, keeps the list itself (so it survives restarts, and
`indra node folders` on it shows the same list), and the console can then start agents there.
For this computer (**local**), the list starts as the folders indra was started with (`--target`,
or the folder you ran `indra serve` in). Saving an empty list goes back to those.

Only admins can browse or change folders. A machine that is offline cannot be browsed; on it,
`indra node folders D:\code\my-app` does the same.

## Day to day

| I want to... | Run on that machine |
|---|---|
| See which hub it joined and whether its link runs | `indra node status` |
| Start the link by hand (after `--foreground`, or with no background service) | `indra node connect` |
| See or change the folders agents may work in | `indra node folders`, or `indra node folders D:\work D:\other` (restarts the link) |
| Read its log | the path `indra node status` prints (Linux with systemd: `journalctl --user -u indra-node`) |
| Leave the hub | `indra node leave` |

To remove a machine from the hub, click **Remove** on its card under **Machines**. It
disconnects at once and its background link stops. To add it again, run a new join command
on it.

## Troubleshooting

**The console shows "Other machines cannot reach this console yet".** It listens on this
computer only. Restart it with `indra serve --bind 0.0.0.0:7878`.

**"no answer from http://... in a minute".** The machine cannot reach the hub at that address.
Open the same address in a browser on that machine. If it does not load, check that both
machines are on the same network, and that the hub computer's firewall lets in port 7878. On
Windows, allow `indra.exe` when Windows asks.

**"the hub refused the code".** The code was mistyped, already used, or older than an hour.
Click **Add a machine** again for a new one.

**"the hub cannot add this machine".** The hub runs read-only or with `--lock-config`, which also
stops machines from joining. Restart it without that option. (Listening on the network does not
stop machines joining. It only stops other settings being edited from a browser, unless you also
pass `--allow-remote-config`.)

**The machine shows "offline".** On it, run `indra node status`. If nothing keeps the link
running, `indra node connect` starts it. If it says the hub no longer knows the machine, it was
removed: join it again with a new code.

**It joined, but has no agents.** Install Claude Code (or Codex, Gemini) on that machine, then
restart its link (`indra node connect`, or sign out and in again). The background service uses
the `PATH` from when you joined, so install agents before joining, or join again afterwards.

## Do I still need SSH?

No. Joining replaces it. SSH remains as an option for machines you already reach that way and
cannot run a link on: add them under **Machines > Advanced (SSH)** (Settings > Nodes) with an
`ssh` destination. A machine with both uses its link when it is connected, and SSH otherwise.
See [Nodes and orchestration](../../tools/indra/README.md#nodes-and-orchestration) for the
details.

## Is it safe?

- The code works once, for an hour, and only an **admin** of the console can make one.
- Each machine gets its own random 256-bit key. Removing the machine deletes the key, and the
  machine can no longer connect.
- A machine runs only the agents in its own configuration, as its own user, in the folders it
  was given. The hub cannot make it run anything else.
- Use `https://` (indra behind a TLS reverse proxy) when the machines are not on the same trusted
  network: the code and the key travel in the first request.
