unit aboutunit;

{$mode objfpc}{$H+}

interface

uses
  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls, ExtCtrls;

type
  TAboutForm = class(TForm)
    IconPanel: TPanel;
    TitleLabel: TLabel;
    VersionLabel: TLabel;
    DescriptionLabel: TLabel;
    CloseButton: TButton;
  end;

var
  AboutForm: TAboutForm;

implementation

{$R *.lfm}

end.
