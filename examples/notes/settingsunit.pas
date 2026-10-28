unit settingsunit;

{$mode objfpc}{$H+}

interface

uses
  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls, ExtCtrls,
  Buttons;

type
  TThemeSetting = (tsSystem, tsLight, tsDark);

  TSettingsForm = class(TForm)
    WordWrapCheckBox: TCheckBox;
    FontSizeLabel: TLabel;
    FontSizeCombo: TComboBox;
    ThemeGroup: TRadioGroup;
    OKButton: TBitBtn;
    CancelButton: TBitBtn;
    procedure OKButtonClick(Sender: TObject);
  public
    procedure LoadSettings(AWordWrap: Boolean; AFontSize: Integer; ATheme: TThemeSetting);
    function WordWrapSetting: Boolean;
    function FontSizeSetting: Integer;
    function ThemeSetting: TThemeSetting;
  end;

var
  SettingsForm: TSettingsForm;

implementation

{$R *.lfm}

const
  DefaultFontSize = 12;

// Shows the current values. Unknown font sizes fall back to DefaultFontSize.
procedure TSettingsForm.LoadSettings(AWordWrap: Boolean; AFontSize: Integer; ATheme: TThemeSetting);
var
  Index: Integer;
begin
  WordWrapCheckBox.Checked := AWordWrap;
  Index := FontSizeCombo.Items.IndexOf(IntToStr(AFontSize));
  if Index < 0 then
    Index := FontSizeCombo.Items.IndexOf(IntToStr(DefaultFontSize));
  FontSizeCombo.ItemIndex := Index;
  ThemeGroup.ItemIndex := Ord(ATheme);
end;

function TSettingsForm.WordWrapSetting: Boolean;
begin
  Result := WordWrapCheckBox.Checked;
end;

function TSettingsForm.FontSizeSetting: Integer;
begin
  Result := StrToIntDef(FontSizeCombo.Text, DefaultFontSize);
end;

function TSettingsForm.ThemeSetting: TThemeSetting;
begin
  case ThemeGroup.ItemIndex of
    1: Result := tsLight;
    2: Result := tsDark;
  else
    Result := tsSystem;
  end;
end;


procedure TSettingsForm.OKButtonClick(Sender: TObject);
var
index: integer;
begin
 index:=1;
end;
end.
