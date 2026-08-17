# ADR 0148: Remote Host Lifecycle, Project Activation, and Narrow Startup Operations

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0117, ADR 0133
Relates to: ADR 0121, ADR 0131

## Context

ADR 0133 intentionally assumes the Windows host is already powered on, connected to the private network, required GameEngine services are running, and the target project is open in its project-scoped Editor when write-capable authoring is needed. Power-on, OS login, Launcher control, project selection, and Editor startup are separate lifecycle concerns.

Remote AI Studio becomes substantially more useful when a user can safely bring an existing project-scoped Editor service into the required ready state without receiving a generic remote shell/process launcher.

## Decision

GameEngine may add narrow authenticated remote lifecycle operations above ADR 0117 application lifecycle. The lifecycle surface is a separate application authority from AgentRun mutation permissions and remains constrained to known GameEngine lifecycle actions.

Supported capability slices may include:

1. report host/Launcher/Editor availability;
2. list only projects known through the user's Launcher/recent-project application state;
3. activate an already-running Editor for a selected canonical project location;
4. request Launcher-managed Editor startup for an explicitly selected project; and
5. report bootstrap readiness/failure back to Remote AI Studio.

Machine wake/power-on may be integrated only through an explicit platform/provider capability with bounded target identity. OS credential entry or automatic interactive login is not part of the GameEngine project lifecycle contract.

## No generic remote launcher

The remote lifecycle API MUST NOT accept arbitrary executable paths, argv, shell commands, environment variables, or filesystem locations. Editor startup uses ADR 0117 Launcher/project-lifecycle contracts and existing project compatibility/lease checks.

If the requested project already has an Editor lease, the existing Editor is activated instead of starting a second writer. If startup fails, the remote client receives sanitized structured lifecycle diagnostics.

## Authentication and network boundary

ADR 0133 private-network reachability remains authoritative. Lifecycle actions require an authenticated remote identity and an explicit lifecycle permission/policy suitable for the operation. Network membership alone does not grant Editor startup.

Machine-specific endpoint, process, wake, and Launcher state remains user/application data and is not written to canonical project data or project-shared AI session records.

## Relationship to AgentRun

Starting/activating the required Editor only establishes application availability. It does not imply Go, grant authoring permissions, resume a cancelled run, or bypass pending permission/AwaitingUser decisions. Once available, normal Remote AI Studio continues through the same Agent Host and project-scoped MCP writer.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141-0147, ADR 0149, ADR 0151, and ADR 0153. It does not depend on engine-native video.

## Verification

Implementation must cover duplicate-open prevention, project lease reuse, explicit project selection, incompatible-project failure, failed Editor bootstrap, no arbitrary process execution surface, sanitized remote errors, no lifecycle credentials in project data, and unchanged AgentRun authorization semantics.

Remote lifecycle UI changes require mobile/narrow-layout Visual Validation in addition to any Launcher/Editor visual evidence.

## Non-goals

This ADR does not implement general remote desktop, arbitrary OS login automation, public Internet ingress, or a universal Wake-on-LAN service. Those require platform/security decisions outside the project-writer lifecycle.
