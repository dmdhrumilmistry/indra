# Rolling indra out with Jamf, Intune and other MDM tools

An organization can put indra on many machines at once, join them all to its hub, and give them
the same settings, with the device management it already uses: Jamf Pro, Microsoft Intune,
Kandji, Workspace ONE, or any tool that can run a script or place a file.

There are two parts. Use either or both:

- **An enrollment key** joins many machines to one hub (instead of a one-time code per machine).
- **Organization settings** give every machine the same defaults, or enforce some settings.

## Join many machines with an enrollment key

1. In the hub's console, go to **Machines > Add many machines**.
2. Give the key a label (for example "engineering laptops"), choose how long it is valid
   (7 days to a year) and, if you like, how many machines may use it. Click **Make a key**.
3. Copy the key, or one of the ready-made scripts below it, right away. indra keeps only a
   fingerprint of the key and cannot show it again.

Each script installs indra for the signed-in user, joins the hub with the key, and keeps the link
running in the background (it starts again at sign-in). The machine is named after its computer
name, and agents may work in the user's home folder (change that later on the Machines page).

The keys are listed on the same panel: how many machines used each one, which machines, and when
it expires. **Revoke** stops a key from adding more machines; machines that already joined stay.

Running a script again on a machine that already joined does nothing (it says so, and makes sure
the link runs), so a policy that re-runs is safe.

### Jamf Pro (macOS)

**Settings > Scripts > New**, paste the **Jamf (macOS)** script from the console, then add it to a
policy (for example: Recurring check-in, Once per computer). Jamf runs scripts as root; the
script installs and joins as the user who is signed in:

```sh
#!/bin/sh
user=$(stat -f%Su /dev/console)
{ [ -z "$user" ] || [ "$user" = root ]; } && exit 0
launchctl asuser "$(id -u "$user")" sudo -u "$user" -H sh -c "cd ~ && curl -fsSL https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.sh | INDRA_JOIN='https://hub.example.com:7878 IEK-...' sh"
```

### Microsoft Intune (Windows)

**Devices > Scripts and remediations > Platform scripts > Add > Windows 10 and later**, upload the
**Intune (Windows)** script from the console, and set **Run this script using the logged on
credentials: Yes** and **Run script in 64 bit PowerShell Host: Yes**:

```powershell
Set-Location $HOME
$env:INDRA_JOIN = "https://hub.example.com:7878 IEK-..."
irm https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.ps1 | iex
```

### Linux and other tools

Run this as the user who will run agents (Ansible `become_user`, a login script, ...):

```sh
cd ~ && curl -fsSL https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.sh | INDRA_JOIN='https://hub.example.com:7878 IEK-...' sh
```

## Organization settings

indra reads two optional files from a folder only administrators can write, besides each user's
own `indra.toml`:

| OS | Folder |
|---|---|
| macOS | `/Library/Application Support/indra` |
| Linux | `/etc/indra` |
| Windows | `C:\ProgramData\indra` |

- **`defaults.toml`** holds starting values. A user's own settings win where they say something
  else.
- **`managed.toml`** holds enforced values. They win over the user's settings. The console's
  Settings page lists them as "Set by your organization", and `indra serve` prints them when it
  starts.

Both are ordinary `indra.toml` files: any section works. How they combine:

- Sections merge key by key (`[local]`, `[routing]`, `[server]`, ...).
- Lists of named entries (`[[runner]]`, `[[profile]]`, `[[account]]`, `[[node]]`,
  `[[workflow]]`, ...) merge by `name`: an entry from a higher layer replaces the one with the same
  name, and others are kept.
- Anything else is replaced by the higher layer.

### Example: the same agents and limits everywhere

`managed.toml`:

```toml
# Every machine routes runs the same way and runs at most two agents at once.
[local]
max_parallel = 2

[routing]
strategy = "most_remaining"

# A profile people cannot change: reviews always run on this model, read-only.
[[profile]]
name = "reviewer"
runner = "claude"
model = "opus"
permission_mode = "plan"

# Join this hub with `indra join` and no arguments.
[join]
hub = "https://hub.example.com:7878"
key = "IEK-..."
```

`defaults.toml`:

```toml
# A local model server to start from; users may point at their own.
[[profile]]
name = "local"
runner = "claude"
provider = "ollama"
ollama_url = "http://gpu.example.com:11434"
model = "qwen3-coder"
```

### Deploying the files

- **Jamf:** a script that writes the file (`mkdir -p "/Library/Application Support/indra"` and a
  heredoc), or a package that installs it.
- **Intune:** a platform script (run as system) that writes `C:\ProgramData\indra\managed.toml`,
  or a Win32 app that installs it.
- **Linux:** your configuration management (Ansible `copy`, Salt `file.managed`, ...).

Keep the folder and files writable only by administrators, so users cannot change enforced
settings. `INDRA_SYSTEM_DIR` points indra at another folder (for testing a layout first).

## Is it safe?

- Only console **admins** can make, list or revoke keys. The hub stores each key's SHA-256, never
  the key, and compares in constant time.
- A key only lets a machine join. It gives no access to the console, and each machine still gets
  its own random key for its link, which **Remove** on the Machines page deletes.
- Keep the expiry short and the machine limit close to the fleet size. Revoke a key once the
  roll-out is done, or if it leaks. Scope secrets to the machines that need them (Settings >
  Secrets), so a machine that should not have joined gets none.
- A `managed.toml` holding a `[join]` key is readable by users of that machine. If that matters,
  run the join script instead, so the key never lands on disk.
