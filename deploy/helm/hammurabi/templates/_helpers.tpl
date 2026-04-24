{{/* Resolved image reference (repository:tag). */}}
{{- define "hammurabi.image" -}}
{{- $registry := .Values.image.registry | default "ghcr.io" -}}
{{- $repo := .Values.image.repository -}}
{{- if eq $repo "" -}}
  {{- $suffix := "" -}}
  {{- if or (eq .Values.agent "claude") (eq .Values.agent "acp_claude") -}}
    {{- $suffix = "-claude" -}}
  {{- else if eq .Values.agent "acp_gemini" -}}
    {{- $suffix = "-gemini" -}}
  {{- else if eq .Values.agent "acp_codex" -}}
    {{- $suffix = "-codex" -}}
  {{- else if eq .Values.agent "none" -}}
    {{- $suffix = "-base" -}}
  {{- else -}}
    {{- fail (printf "Unknown .Values.agent %q — must be one of claude|acp_claude|acp_gemini|acp_codex|none" .Values.agent) -}}
  {{- end -}}
  {{- $repo = printf "hydai/hammurabi%s" $suffix -}}
{{- end -}}
{{- $tag := .Values.image.tag | default .Chart.AppVersion -}}
{{- printf "%s/%s:%s" $registry $repo $tag -}}
{{- end -}}

{{/* Secret name (generated or externally managed). */}}
{{- define "hammurabi.secretName" -}}
{{- if .Values.secrets.create -}}
{{- printf "%s-secrets" .Release.Name -}}
{{- else -}}
{{- required ".Values.secrets.existingSecret is required when .Values.secrets.create is false" .Values.secrets.existingSecret -}}
{{- end -}}
{{- end -}}

{{/* Common labels. */}}
{{- define "hammurabi.labels" -}}
app.kubernetes.io/name: hammurabi
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{- define "hammurabi.selectorLabels" -}}
app.kubernetes.io/name: hammurabi
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* Rendered hammurabi.toml: either the literal from values.raw (after
     `tpl` expansion) or a bootstrap stub pointing at the remote URL. */}}
{{- define "hammurabi.renderedConfig" -}}
{{- if and (ne (trim .Values.config.raw) "") (ne (trim .Values.config.url) "") -}}
{{- fail "Set exactly one of .Values.config.raw or .Values.config.url" -}}
{{- else if ne (trim .Values.config.raw) "" -}}
{{ tpl .Values.config.raw . }}
{{- else if ne (trim .Values.config.url) "" -}}
# Remote config — the daemon fetches the actual TOML from the URL at
# startup and each poll cycle. This stub keeps the ConfigMap non-empty.
remote_url = "{{ .Values.config.url }}"
{{- else -}}
{{- fail "Neither .Values.config.raw nor .Values.config.url is set" -}}
{{- end -}}
{{- end -}}
