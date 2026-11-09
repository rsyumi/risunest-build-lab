param([uint32]$RootProcessId = 0, [switch]$Stream)
$ErrorActionPreference = 'Stop'
$collectorSource = @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace RisuNestStartupMemory {
    public sealed class NativeHandle : SafeHandleZeroOrMinusOneIsInvalid {
        public NativeHandle() : base(true) { }
        protected override bool ReleaseHandle() { return Native.CloseHandle(handle); }
    }
    static class Native {
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        internal struct Entry {
            public uint Size, Usage, Id;
            public UIntPtr Heap;
            public uint Module, Threads, Parent;
            public int Priority;
            public uint Flags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string Name;
        }
        [StructLayout(LayoutKind.Sequential)]
        internal struct Counters {
            public uint Size, Faults;
            public UIntPtr Peak, Working, PeakPaged, Paged, PeakNonPaged, NonPaged, Pagefile, PeakPagefile, Private;
        }
        [DllImport("kernel32.dll", SetLastError = true)] internal static extern NativeHandle CreateToolhelp32Snapshot(uint flags, uint pid);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool Process32FirstW(NativeHandle snapshot, ref Entry entry);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool Process32NextW(NativeHandle snapshot, ref Entry entry);
        [DllImport("kernel32.dll", SetLastError = true)] internal static extern NativeHandle OpenProcess(uint rights, [MarshalAs(UnmanagedType.Bool)] bool inherit, uint pid);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool GetProcessTimes(NativeHandle process, out long created, out long exited, out long kernel, out long user);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool K32GetProcessMemoryInfo(NativeHandle process, ref Counters counters, uint size);
        [DllImport("kernel32.dll", SetLastError = true)] internal static extern uint WaitForSingleObject(NativeHandle process, uint milliseconds);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll")] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool QueryPerformanceCounter(out long value);
        [DllImport("kernel32.dll")] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool QueryPerformanceFrequency(out long value);
    }
    public sealed class Relation {
        public uint Id, Parent;
        public Relation(uint id, uint parent) { Id = id; Parent = parent; }
    }
    public sealed class Memory {
        public long Working, Private, LifetimePeak;
        public Memory(long working, long privateBytes, long peak) { Working = working; Private = privateBytes; LifetimePeak = peak; }
    }
    public abstract class ProcessLease : IDisposable {
        public abstract long Created { get; }
        public abstract bool Alive { get; }
        public abstract Memory Read();
        public abstract void Dispose();
    }
    public interface ProcessSource {
        double Now { get; }
        List<Relation> Snapshot();
        ProcessLease Open(uint pid);
    }
    sealed class NativeLease : ProcessLease {
        NativeHandle handle;
        readonly long created;
        public NativeLease(uint pid) {
            // Limited query plus synchronize, without VM_READ or debug privilege.
            handle = Native.OpenProcess(0x1000 | 0x100000, false, pid);
            if (handle.IsInvalid) { int error = Marshal.GetLastWin32Error(); handle.Dispose(); throw new Win32Exception(error); }
            long exited, kernel, user;
            if (!Native.GetProcessTimes(handle, out created, out exited, out kernel, out user)) {
                int error = Marshal.GetLastWin32Error(); handle.Dispose(); throw new Win32Exception(error);
            }
        }
        public override long Created { get { return created; } }
        public override bool Alive {
            get {
                uint status = Native.WaitForSingleObject(handle, 0);
                if (status == 258) return true;
                if (status == 0) return false;
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
        }
        public override Memory Read() {
            Native.Counters counters = new Native.Counters();
            counters.Size = (uint)Marshal.SizeOf(typeof(Native.Counters));
            if (!Native.K32GetProcessMemoryInfo(handle, ref counters, counters.Size)) throw new Win32Exception(Marshal.GetLastWin32Error());
            return new Memory(checked((long)counters.Working.ToUInt64()), checked((long)counters.Private.ToUInt64()), checked((long)counters.Peak.ToUInt64()));
        }
        public override void Dispose() { handle.Dispose(); }
    }
    public sealed class NativeSource : ProcessSource {
        static readonly long frequency;
        static readonly long origin;
        static NativeSource() {
            if (!Native.QueryPerformanceFrequency(out frequency) || frequency <= 0 || !Native.QueryPerformanceCounter(out origin)) throw new InvalidOperationException("Native clock unavailable");
        }
        public static long Frequency { get { return frequency; } }
        public double Now { get { long value; Native.QueryPerformanceCounter(out value); return (value - origin) * 1000.0 / frequency; } }
        public ProcessLease Open(uint pid) { return new NativeLease(pid); }
        public List<Relation> Snapshot() {
            using (NativeHandle snapshot = Native.CreateToolhelp32Snapshot(2, 0)) {
                if (snapshot.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
                Native.Entry entry = new Native.Entry();
                entry.Size = (uint)Marshal.SizeOf(typeof(Native.Entry));
                List<Relation> rows = new List<Relation>();
                bool next = Native.Process32FirstW(snapshot, ref entry);
                while (next) { rows.Add(new Relation(entry.Id, entry.Parent)); next = Native.Process32NextW(snapshot, ref entry); }
                int error = Marshal.GetLastWin32Error();
                if (error != 18) throw new Win32Exception(error);
                return rows;
            }
        }
        public static void Warm(uint self) {
            NativeSource source = new NativeSource();
            source.Snapshot();
            using (ProcessLease process = source.Open(self)) { if (!process.Alive) throw new InvalidOperationException(); process.Read(); }
            using (Collector collector = new Collector(source)) { collector.Bind(self); collector.Read(); }
        }
    }
    public sealed class SampleError {
        public string Code;
        public int Win32Error;
        public SampleError(string code, Exception error) { Code = code; Win32Exception native = error as Win32Exception; Win32Error = native == null ? 0 : native.NativeErrorCode; }
    }
    public sealed class Sample {
        public bool Complete;
        public double NativeStartMs, SnapshotStartMs, SnapshotEndMs, NativeEndMs, EnumerationMs;
        public int SnapshotCount;
        public long? NativeWorkingSetBytes, NativePrivateBytes, NativePeakWorkingSetBytes, TreeWorkingSetBytes, TreePrivateBytes;
        public int? ProcessCount;
        public int KnownCount, QueriedCount, ExitedCount, IdentityRefusedCount;
        public List<SampleError> Errors = new List<SampleError>();
    }
    public sealed class Collector : IDisposable {
        sealed class Member { public ProcessLease Lease; public long Created; }
        readonly ProcessSource source;
        readonly Dictionary<uint, Member> members = new Dictionary<uint, Member>();
        uint root;
        bool disposed;
        public Collector(ProcessSource source) { this.source = source; }
        public void Bind(uint pid, uint owner = 0) {
            if (disposed || root != 0 || pid == 0) throw new InvalidOperationException("Invalid root binding");
            ProcessLease process = source.Open(pid);
            try {
                if (!process.Alive) throw new InvalidOperationException("Root exited before binding");
                if (owner != 0) {
                    bool owned = source.Snapshot().Exists(row => row.Id == pid && row.Parent == owner);
                    if (!owned || !process.Alive) throw new InvalidOperationException("Root is not the launched child");
                }
                members.Add(pid, new Member { Lease = process, Created = process.Created });
                root = pid;
            } catch { process.Dispose(); throw; }
        }
        public Sample Read() {
            if (disposed || root == 0) throw new InvalidOperationException("Collector is not bound");
            Sample sample = new Sample();
            sample.NativeStartMs = source.Now;
            sample.SnapshotStartMs = sample.NativeStartMs;
            sample.SnapshotEndMs = sample.NativeStartMs;
            try {
                foreach (Member member in members.Values) {
                    if (member.Lease != null && !member.Lease.Alive) { member.Lease.Dispose(); member.Lease = null; sample.ExitedCount++; }
                }
                if (members[root].Lease == null) throw new InvalidOperationException("Root exited");
                sample.SnapshotStartMs = source.Now;
                sample.SnapshotCount++;
                List<Relation> rows;
                try { rows = source.Snapshot(); }
                finally {
                    sample.SnapshotEndMs = source.Now;
                    sample.EnumerationMs += sample.SnapshotEndMs - sample.SnapshotStartMs;
                }
                HashSet<uint> present = new HashSet<uint>();
                foreach (Relation row in rows) present.Add(row.Id);
                HashSet<uint> attempted = new HashSet<uint>();
                bool added;
                do {
                    added = false;
                    foreach (Relation row in rows) {
                        Member existing = null, parent;
                        if (row.Id == root || members.TryGetValue(row.Id, out existing) && existing.Lease != null || attempted.Contains(row.Id)) continue;
                        if (!members.TryGetValue(row.Parent, out parent)) continue;
                        attempted.Add(row.Id);
                        if (parent.Lease == null) { sample.Errors.Add(new SampleError("ambiguous-parent", null)); continue; }
                        ProcessLease child = null;
                        try {
                            if (!parent.Lease.Alive) { sample.Errors.Add(new SampleError("parent-exited", null)); continue; }
                            child = source.Open(row.Id);
                            if (child.Created < parent.Created || existing != null && child.Created == existing.Created) {
                                sample.IdentityRefusedCount++;
                                sample.Errors.Add(new SampleError("identity-refused", null));
                                continue;
                            }
                            if (!child.Alive) { sample.Errors.Add(new SampleError("child-exited", null)); continue; }
                            double validationStart = source.Now;
                            List<Relation> current;
                            sample.SnapshotCount++;
                            try { current = source.Snapshot(); }
                            finally { sample.EnumerationMs += source.Now - validationStart; }
                            if (!current.Exists(candidate => candidate.Id == row.Id && candidate.Parent == row.Parent)
                                || !parent.Lease.Alive || !child.Alive) {
                                sample.IdentityRefusedCount++;
                                sample.Errors.Add(new SampleError("identity-refused", null));
                                continue;
                            }
                            if (members.Count >= 4096) throw new InvalidOperationException("Tracked identity limit");
                            members[row.Id] = new Member { Lease = child, Created = child.Created };
                            child = null;
                            added = true;
                        } catch (Exception error) { sample.Errors.Add(new SampleError("child-open-failed", error)); }
                        finally { if (child != null) child.Dispose(); }
                    }
                } while (added);
                long working = 0, privateBytes = 0;
                Memory nativeMemory = null;
                foreach (KeyValuePair<uint, Member> pair in members) {
                    ProcessLease lease = pair.Value.Lease;
                    if (lease == null) continue;
                    sample.KnownCount++;
                    try {
                        if (!present.Contains(pair.Key)) { sample.Errors.Add(new SampleError("missing-from-snapshot", null)); continue; }
                        if (!lease.Alive) { sample.Errors.Add(new SampleError("exited-during-sweep", null)); continue; }
                        Memory memory = lease.Read();
                        working = checked(working + memory.Working);
                        privateBytes = checked(privateBytes + memory.Private);
                        sample.QueriedCount++;
                        if (pair.Key == root) nativeMemory = memory;
                    } catch (Exception error) { sample.Errors.Add(new SampleError("counter-query-failed", error)); }
                }
                foreach (Member member in members.Values) {
                    if (member.Lease != null && !member.Lease.Alive) sample.Errors.Add(new SampleError("exited-during-sweep", null));
                }
                sample.Complete = sample.Errors.Count == 0 && nativeMemory != null && sample.KnownCount == sample.QueriedCount;
                if (sample.Complete) {
                    sample.NativeWorkingSetBytes = nativeMemory.Working;
                    sample.NativePrivateBytes = nativeMemory.Private;
                    sample.NativePeakWorkingSetBytes = nativeMemory.LifetimePeak;
                    sample.TreeWorkingSetBytes = working;
                    sample.TreePrivateBytes = privateBytes;
                    sample.ProcessCount = sample.QueriedCount;
                }
            } catch (Exception error) { sample.Errors.Add(new SampleError("sweep-failed", error)); }
            if (sample.SnapshotEndMs < sample.SnapshotStartMs) sample.SnapshotEndMs = source.Now;
            sample.NativeEndMs = source.Now;
            return sample;
        }
        public void Dispose() {
            if (disposed) return;
            disposed = true;
            foreach (Member member in members.Values) if (member.Lease != null) member.Lease.Dispose();
            members.Clear();
        }
    }
}
'@
function Write-Reply($value) {
    [Console]::WriteLine(($value | ConvertTo-Json -Depth 6 -Compress))
    [Console]::Out.Flush()
}
function Write-Sample($sample) {
    Write-Reply ([ordered]@{
        kind = 'sample'; complete = $sample.Complete
        nativeStartMs = $sample.NativeStartMs; snapshotStartMs = $sample.SnapshotStartMs
        snapshotEndMs = $sample.SnapshotEndMs; nativeEndMs = $sample.NativeEndMs
        enumerationMs = $sample.EnumerationMs; snapshotCount = $sample.SnapshotCount
        nativeWorkingSetBytes = $sample.NativeWorkingSetBytes; nativePrivateBytes = $sample.NativePrivateBytes
        nativePeakWorkingSetBytes = $sample.NativePeakWorkingSetBytes
        treeWorkingSetBytes = $sample.TreeWorkingSetBytes; treePrivateBytes = $sample.TreePrivateBytes
        processCount = $sample.ProcessCount; knownCount = $sample.KnownCount; queriedCount = $sample.QueriedCount
        exitedCount = $sample.ExitedCount; identityRefusedCount = $sample.IdentityRefusedCount
        errors = @($sample.Errors | ForEach-Object { @{ code = $_.Code; win32Error = $_.Win32Error } })
    })
}
$collector = $null
try {
    Add-Type -TypeDefinition $collectorSource -WarningAction SilentlyContinue
    [RisuNestStartupMemory.NativeSource]::Warm([uint32]$PID)
    $collector = [RisuNestStartupMemory.Collector]::new([RisuNestStartupMemory.NativeSource]::new())
    if ($RootProcessId) { $collector.Bind($RootProcessId) }
    if ($Stream) {
        Write-Reply @{ kind = 'ready'; protocol = 2; nativeFrequency = [RisuNestStartupMemory.NativeSource]::Frequency }
        while ($null -ne ($request = [Console]::ReadLine())) {
            if ($request -eq 'stop') { break }
            if ($request -match '^bind ([1-9][0-9]*) ([1-9][0-9]*)$') { $collector.Bind([uint32]$Matches[1], [uint32]$Matches[2]); Write-Reply @{ kind = 'bound' }; continue }
            if ($request -ne 'sample') { throw 'Unknown memory sampler request' }
            Write-Sample ($collector.Read())
        }
    } else {
        if (!$RootProcessId) { throw 'A root PID is required' }
        Write-Sample ($collector.Read())
    }
} catch {
    Write-Reply @{ kind = 'failure'; code = 'native-helper-failed' }
    exit 1
} finally {
    if ($null -ne $collector) { $collector.Dispose() }
}
