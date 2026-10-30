unit PascodaVlc;

{$mode objfpc}{$H+}

// Video and audio players for Pascoda, on the VLC library that is installed on the Mac
// (/Applications/VLC.app). The library is loaded when a player first needs it, so a project that
// only builds with this package still builds on a Mac without VLC; playing then raises
// EPascodaVlc with the reason.
//
// Put a TPanel on the form and set the player's VideoPanel to it: the video is drawn in the panel.

interface

uses
  Classes, SysUtils, Controls, dynlibs, ctypes, libvlc;

type
  EPascodaVlc = class(Exception);

  { A player for one file at a time. }
  TPascodaVlcPlayer = class(TComponent)
  private
    FVideoPanel: TWinControl;
    FPlayer: plibvlc_media_player_t;
    FMedia: plibvlc_media_t;
    procedure SetVideoPanel(AValue: TWinControl);
    procedure AttachVideo;
  protected
    procedure Notification(AComponent: TComponent; Operation: TOperation); override;
  public
    destructor Destroy; override;
    // Opens a media file (the path on this Mac), ready for Play.
    procedure Open(const FileName: string);
    procedure Play;
    procedure Pause;
    procedure Stop;
    function Playing: Boolean;
  published
    // The panel the video is drawn in. Leave it unset for audio only.
    property VideoPanel: TWinControl read FVideoPanel write SetVideoPanel;
  end;

  { A player for a list of files: PlayNext plays the next one, and starts again at the first. }
  TPascodaVlcListPlayer = class(TPascodaVlcPlayer)
  private
    FFiles: TStringList;
    FIndex: Integer;
  public
    constructor Create(AOwner: TComponent); override;
    destructor Destroy; override;
    procedure PlayNext;
    // The files to play, in order. Set them in code (Files.Add) before PlayNext.
    property Files: TStringList read FFiles;
  end;

// True when the VLC library can be found on this Mac.
function PascodaVlcInstalled: Boolean;

implementation

{$ifdef darwin}
function setenv(name, value: PAnsiChar; overwrite: longint): longint; cdecl; external 'c' name 'setenv';
{$endif}

const
  // Where VLC.app keeps its library, first found wins. PASCODA_VLC_LIB overrides the search.
  VlcLibCandidates: array[0..2] of string = (
    '/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib',
    '~/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib',
    '/opt/homebrew/lib/libvlc.dylib');

var
  VlcInstance: plibvlc_instance_t = nil;
  VlcLoaded: Boolean = False;

function FindLibrary: string;
var
  i: Integer;
  env: string;
begin
  env := GetEnvironmentVariable('PASCODA_VLC_LIB');
  if (env <> '') and FileExists(env) then
    Exit(env);
  for i := Low(VlcLibCandidates) to High(VlcLibCandidates) do
  begin
    Result := ExpandFileName(StringReplace(VlcLibCandidates[i], '~', GetUserDir, []));
    if FileExists(Result) then
      Exit;
  end;
  Result := '';
end;

function PascodaVlcInstalled: Boolean;
begin
  Result := FindLibrary <> '';
end;

// Loads the library and creates the VLC instance, once. Raises EPascodaVlc when VLC is missing.
procedure EnsureLoaded;
var
  lib, dir: string;
begin
  if VlcLoaded then
    Exit;
  lib := FindLibrary;
  if lib = '' then
    raise EPascodaVlc.Create('VLC is not installed: install VLC.app (videolan.org) to play media');
  dir := ExtractFilePath(lib);
  // libvlc.dylib refers to its core through @rpath: the core is loaded by its full path first.
  {$ifdef darwin}
  if LoadLibrary(dir + 'libvlccore.dylib') = NilHandle then
    raise EPascodaVlc.Create('VLC core library could not be loaded from ' + dir);
  setenv('VLC_PLUGIN_PATH', PAnsiChar(dir + '../plugins'), 1);
  {$endif}
  Loadlibvlc(lib, False);
  VlcInstance := libvlc_new(0, nil);
  if VlcInstance = nil then
    raise EPascodaVlc.Create('VLC did not start (the library could not create an instance)');
  VlcLoaded := True;
end;

{ TPascodaVlcPlayer }

destructor TPascodaVlcPlayer.Destroy;
begin
  if FPlayer <> nil then
    libvlc_media_player_release(FPlayer);
  if FMedia <> nil then
    libvlc_media_release(FMedia);
  inherited Destroy;
end;

procedure TPascodaVlcPlayer.Notification(AComponent: TComponent; Operation: TOperation);
begin
  inherited Notification(AComponent, Operation);
  if (Operation = opRemove) and (AComponent = FVideoPanel) then
    FVideoPanel := nil;
end;

procedure TPascodaVlcPlayer.SetVideoPanel(AValue: TWinControl);
begin
  if FVideoPanel = AValue then
    Exit;
  FVideoPanel := AValue;
  if Assigned(FVideoPanel) then
    FVideoPanel.FreeNotification(Self);
  AttachVideo;
end;

// Hands the panel's native handle to VLC: an NSView on macOS, an X window or HWND elsewhere.
procedure TPascodaVlcPlayer.AttachVideo;
begin
  if (FPlayer = nil) or not Assigned(FVideoPanel) or not FVideoPanel.HandleAllocated then
    Exit;
  {$ifdef darwin}
  libvlc_media_player_set_nsobject(FPlayer, Pointer(FVideoPanel.Handle));
  {$else}
  {$ifdef windows}
  libvlc_media_player_set_hwnd(FPlayer, Pointer(FVideoPanel.Handle));
  {$else}
  libvlc_media_player_set_xwindow(FPlayer, uint32_t(FVideoPanel.Handle));
  {$endif}
  {$endif}
end;

procedure TPascodaVlcPlayer.Open(const FileName: string);
begin
  EnsureLoaded;
  if FMedia <> nil then
  begin
    libvlc_media_release(FMedia);
    FMedia := nil;
  end;
  FMedia := libvlc_media_new_path(VlcInstance, pcchar(PAnsiChar(FileName)));
  if FMedia = nil then
    raise EPascodaVlc.Create('VLC cannot open ' + FileName);
  if FPlayer = nil then
    FPlayer := libvlc_media_player_new(VlcInstance);
  libvlc_media_player_set_media(FPlayer, FMedia);
  AttachVideo;
end;

procedure TPascodaVlcPlayer.Play;
begin
  if FPlayer = nil then
    raise EPascodaVlc.Create('No file is open: call Open first');
  AttachVideo;
  libvlc_media_player_play(FPlayer);
end;

procedure TPascodaVlcPlayer.Pause;
begin
  if FPlayer <> nil then
    libvlc_media_player_pause(FPlayer);
end;

procedure TPascodaVlcPlayer.Stop;
begin
  if FPlayer <> nil then
    libvlc_media_player_stop(FPlayer);
end;

function TPascodaVlcPlayer.Playing: Boolean;
begin
  Result := (FPlayer <> nil) and (libvlc_media_player_is_playing(FPlayer) <> 0);
end;

{ TPascodaVlcListPlayer }

constructor TPascodaVlcListPlayer.Create(AOwner: TComponent);
begin
  inherited Create(AOwner);
  FFiles := TStringList.Create;
  FIndex := -1;
end;

destructor TPascodaVlcListPlayer.Destroy;
begin
  FFiles.Free;
  inherited Destroy;
end;

procedure TPascodaVlcListPlayer.PlayNext;
begin
  if FFiles.Count = 0 then
    raise EPascodaVlc.Create('The list is empty');
  Inc(FIndex);
  if FIndex >= FFiles.Count then
    FIndex := 0;
  Open(FFiles[FIndex]);
  Play;
end;

end.
