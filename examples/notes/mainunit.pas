unit mainunit;

{$mode objfpc}{$H+}
{$IFDEF DARWIN}
{$modeswitch objectivec1}
{$ENDIF}
 
interface

uses
  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, Menus, StdCtrls,
  ComCtrls, ExtCtrls, ActnList, settingsunit;

type
  TMainForm = class(TForm)
    ActionList1: TActionList;
    ActionSelectAll: TAction;
    ActionInsertTime: TAction;
    MainMenu1: TMainMenu;
    FileMenu: TMenuItem;
    FileNewItem: TMenuItem;
    FileOpenItem: TMenuItem;
    FileSaveItem: TMenuItem;
    FileSaveAsItem: TMenuItem;
    FileSep1: TMenuItem;
    FileQuitItem: TMenuItem;
    EditMenu: TMenuItem;
    EditSelectAllItem: TMenuItem;
    EditInsertTimeItem: TMenuItem;
    EditSep1: TMenuItem;
    EditClearItem: TMenuItem;
    ToolsMenu: TMenuItem;
    ToolsSettingsItem: TMenuItem;
    HelpMenu: TMenuItem;
    HelpAboutItem: TMenuItem;
    MemoPopup: TPopupMenu;
    PopupSelectAllItem: TMenuItem;
    PopupClearItem: TMenuItem;
    Memo1: TMemo;
    StatusBar1: TStatusBar;
    Timer1: TTimer;
    OpenDialog1: TOpenDialog;
    SaveDialog1: TSaveDialog;
    procedure ActionInsertTimeExecute(Sender: TObject);
    procedure ActionSelectAllExecute(Sender: TObject);
    procedure EditClearItemClick(Sender: TObject);
    procedure FileNewItemClick(Sender: TObject);
    procedure FileOpenItemClick(Sender: TObject);
    procedure FileQuitItemClick(Sender: TObject);
    procedure FileSaveAsItemClick(Sender: TObject);
    procedure FileSaveItemClick(Sender: TObject);
    procedure HelpAboutItemClick(Sender: TObject);
    procedure MainFormCloseQuery(Sender: TObject; var CanClose: Boolean);
    procedure MainFormShow(Sender: TObject);
    procedure Memo1Change(Sender: TObject);
    procedure Timer1Timer(Sender: TObject);
    procedure ToolsSettingsItemClick(Sender: TObject);
  private
    FFileName: string;
    FDirty: Boolean;
    FLoading: Boolean;
    FTheme: TThemeSetting;
    function ConfirmDiscard: Boolean;
    procedure LoadFile(const AFileName: string);
    procedure NewDocument;
    procedure SaveFile(const AFileName: string);
    procedure UpdateView;
  public
    procedure ApplySettings(AWordWrap: Boolean; AFontSize: Integer; ATheme: TThemeSetting);
  end;

var
  MainForm: TMainForm;

implementation

uses
  LazUTF8, aboutunit
  {$IFDEF DARWIN}
  , CocoaAll, Cocoa_Extra
  {$ENDIF};

{$R *.lfm}

// Words are runs of characters that are not blank.
function CountWords(const AText: string): Integer;
var
  I: Integer;
  InWord: Boolean;
begin
  Result := 0;
  InWord := False;
  for I := 1 to Length(AText) do
    if AText[I] in [#0..' '] then
      InWord := False
    else if not InWord then
    begin
      InWord := True;
      Inc(Result);
    end;
end;

// Asks before throwing away unsaved text. True when the text may be replaced.
function TMainForm.ConfirmDiscard: Boolean;
begin
  Result := (not FDirty) or
    (MessageDlg('Discard the unsaved changes?', mtConfirmation, [mbYes, mbNo], 0) = mrYes);
end;

procedure TMainForm.LoadFile(const AFileName: string);
begin
  FLoading := True;  // loading the text is not an edit
  try
    Memo1.Lines.LoadFromFile(AFileName);
  finally
    FLoading := False;
  end;
  FFileName := AFileName;
  FDirty := False;
  UpdateView;
end;

procedure TMainForm.NewDocument;
begin
  FLoading := True;
  try
    Memo1.Clear;
  finally
    FLoading := False;
  end;
  FFileName := '';
  FDirty := False;
  UpdateView;
end;

procedure TMainForm.SaveFile(const AFileName: string);
begin
  Memo1.Lines.SaveToFile(AFileName);
  FFileName := AFileName;
  FDirty := False;
  UpdateView;
end;

// Window title, word and character counts.
procedure TMainForm.UpdateView;
var
  Title: string;
begin
  if FFileName = '' then
    Title := 'Untitled'
  else
    Title := ExtractFileName(FFileName);
  if FDirty then
    Title := Title + ' (edited)';
  Caption := Title + ' - Notes';
  StatusBar1.Panels[0].Text := Format('Words: %d   Characters: %d',
    [CountWords(Memo1.Text), UTF8Length(Memo1.Text)]);
end;

procedure TMainForm.ApplySettings(AWordWrap: Boolean; AFontSize: Integer; ATheme: TThemeSetting);
{$IFDEF DARWIN}
var
  AppearanceName: NSString;
{$ENDIF}
begin
  FTheme := ATheme;
  {$IFDEF DARWIN}
  case ATheme of
    tsSystem:
      NSApplication.sharedApplication.setAppearance(nil);
    tsLight:
      begin
        AppearanceName := NSString.stringWithUTF8String(PChar('NSAppearanceNameAqua'));
        NSApplication.sharedApplication.setAppearance(NSAppearance.appearanceNamed(AppearanceName));
      end;
    tsDark:
      begin
        AppearanceName := NSString.stringWithUTF8String(PChar('NSAppearanceNameDarkAqua'));
        NSApplication.sharedApplication.setAppearance(NSAppearance.appearanceNamed(AppearanceName));
      end;
  end;
  {$ENDIF}
  Memo1.WordWrap := AWordWrap;
  Memo1.Font.Size := AFontSize;
end;

procedure TMainForm.ActionInsertTimeExecute(Sender: TObject);
begin
  Memo1.SetFocus;
  Memo1.SelText := FormatDateTime('hh:nn', Now);
end;

procedure TMainForm.ActionSelectAllExecute(Sender: TObject);
begin
  Memo1.SetFocus;
  Memo1.SelectAll;
end;

procedure TMainForm.EditClearItemClick(Sender: TObject);
begin
  Memo1.Clear;
end;

procedure TMainForm.FileNewItemClick(Sender: TObject);
begin
  if ConfirmDiscard then
    NewDocument;
end;

procedure TMainForm.FileOpenItemClick(Sender: TObject);
begin
  if not ConfirmDiscard then
    Exit;
  if OpenDialog1.Execute then
    LoadFile(OpenDialog1.FileName);
end;

procedure TMainForm.FileQuitItemClick(Sender: TObject);
begin
  // Terminate skips OnCloseQuery, so ask here as well.
  if ConfirmDiscard then
    Application.Terminate;
end;

procedure TMainForm.FileSaveAsItemClick(Sender: TObject);
begin
  if FFileName <> '' then
    SaveDialog1.FileName := FFileName;
  if SaveDialog1.Execute then
    SaveFile(SaveDialog1.FileName);
end;

procedure TMainForm.FileSaveItemClick(Sender: TObject);
begin
  if FFileName = '' then
    FileSaveAsItemClick(Sender)
  else
    SaveFile(FFileName);
end;

procedure TMainForm.HelpAboutItemClick(Sender: TObject);
begin
  AboutForm.ShowModal;
end;

procedure TMainForm.MainFormCloseQuery(Sender: TObject; var CanClose: Boolean);
begin
  CanClose := ConfirmDiscard;
end;

procedure TMainForm.MainFormShow(Sender: TObject);
begin
  UpdateView;
end;

procedure TMainForm.Memo1Change(Sender: TObject);
begin
  if FLoading then
    Exit;
  FDirty := True;
  UpdateView;
end;

procedure TMainForm.Timer1Timer(Sender: TObject);
begin
  StatusBar1.Panels[1].Text := TimeToStr(Now);
end;

procedure TMainForm.ToolsSettingsItemClick(Sender: TObject);
begin
  SettingsForm.LoadSettings(Memo1.WordWrap, Memo1.Font.Size, FTheme);
  if SettingsForm.ShowModal = mrOk then
    ApplySettings(SettingsForm.WordWrapSetting, SettingsForm.FontSizeSetting,
      SettingsForm.ThemeSetting);
end;

end.
